use std::time::Duration;

use async_recursion::async_recursion;
use futures::TryFutureExt;
use thirtyfour::{components::SelectElement, prelude::*};

use crate::{
    environment::Environment,
    parser::{Cmd, CmdParam, CmdStmt, IfStmt, SetVariableStmt, Stmt},
};

/// Represent the Severity of an error within the interpreter (i.e. how to respond to an error).
/// On a Recoverable error, the script will go to the next catch-error: stmt.
/// On an Exit error, the interpret method will early return.
pub enum Severity {
    Exit,
    Recoverable,
}

/// Type alias for errors in the interpreter.
pub type RuntimeResult<T, E> = Result<T, (E, Severity)>;

/// The interpreter is responsible for executing Schnauzer UI stmts against a running selenium grid.
pub struct Interpreter {
    /// Each interpreter has it's own browser window for executing scripts
    pub driver: WebDriver,

    /// The statements for the interpreter to execute
    stmts: Vec<Stmt>,

    /// Each interpreter gets an environment for storing variables
    environment: Environment,

    /// The locate command brings an element into focus. That element is stored here. Subsequents commands are performed
    /// against this element.
    curr_elem: Option<WebElement>,

    /// The had error field tracks whether or not the script encountered an error, and is used to move between catch-error: statements.
    had_error: bool,

    /// We store the statements that we encounter since the last catch-error stmt in order for
    /// the try-again command to be able to re-execute them.
    stmts_since_last_error_handling: Vec<Stmt>,

    /// The tried again field stores whether or not we are in try-again mode. It is used to cause an early return
    /// in the case that we encounter an error while in try-again mode.
    tried_again: bool,

    /// The progress of the program is stored into a buffer to optionally be written to a file
    pub log_buffer: String,
    pub screenshot_buffer: Vec<Vec<u8>>,

    /// Denotes whether the program is in "demo" mode
    is_demo: bool,
}

impl Interpreter {
    /// Constructor for the Interpreter. Registers a webdriver against a standalone selenium grid running at port 4444.
    pub fn new(driver: WebDriver, stmts: Vec<Stmt>, is_demo: bool) -> Self {
        let stmts = stmts.into_iter().rev().collect();

        Self {
            driver,
            stmts,
            environment: Environment::new(),
            curr_elem: None,
            had_error: false,
            stmts_since_last_error_handling: vec![],
            tried_again: false,
            log_buffer: String::new(),
            screenshot_buffer: vec![],
            is_demo,
        }
    }

    fn log_cmd(&mut self, msg: &str) {
        self.log_buffer.push_str(&format!("Info: {}", msg));
        self.log_buffer.push_str("\n");
    }

    fn log_err(&mut self, msg: &str) {
        self.log_buffer.push_str(&format!("Error: {}", msg));
        self.log_buffer.push_str("\n");
    }

    /// Executes a list of stmts. Returns a boolean indication of whether or not there was an early return.
    pub async fn interpret(&mut self, close_driver: bool) -> WebDriverResult<bool> {
        // Reset in case the interpreter is being reused
        self.curr_elem = None;
        self.had_error = false;
        self.stmts_since_last_error_handling.clear();
        self.tried_again = false;

        while let Some(stmt) = self.stmts.pop() {
            if !self.had_error {
                self.log_cmd(&stmt.to_string());
            }

            if let Err((e, sev)) = self.execute_stmt(stmt).await {
                match sev {
                    Severity::Exit => {
                        self.log_err(&e);
                        if close_driver {
                            self.driver.close_window().await?;
                        }
                        return Ok(true);
                    }
                    Severity::Recoverable => {
                        self.log_err(&e);
                        self.had_error = true;
                    }
                }
            }
        }

        // We completed the entire script.
        if close_driver {
            self.driver.close_window().await?;
        }

        // Return whether or not we exited the program while inn error mode.
        Ok(self.had_error)
    }

    /// Produces an error with the appropriate severity based on
    /// whether we are currently trying to execute stmts again.
    fn error(&self, msg: &str) -> (String, Severity) {
        if self.tried_again {
            (msg.to_owned(), Severity::Exit)
        } else {
            (msg.to_owned(), Severity::Recoverable)
        }
    }

    /// Takes a webelement, attempts to scroll the element into view, and then sets
    /// the element as currently in focus. Subsequent commands will be executed against this element.
    async fn set_curr_elem(
        &mut self,
        elem: WebElement,
        scroll_into_view: bool,
    ) -> RuntimeResult<(), String> {
        // Scroll the element into view if specified
        if scroll_into_view {
            elem.scroll_into_view()
                .await
                .map_err(|_| self.error("Error scrolling web element into view"))?;
        }

        // Give the located element a purple border if in demo mode
        if self.is_demo {
            self.driver
                .execute(
                    r#"
            arguments[0].style.border = '5px solid purple';
            "#,
                    vec![elem
                        .to_json()
                        .map_err(|_| self.error("Error jsonifying element"))?],
                )
                .await
                .map_err(|_| self.error("Error highlighting element"))?;

            // Remove the border from the previously located element
            if let Some(ref curr_elem) = self.curr_elem {
                // For now we are explicitly ignoring the error, because if the un-highlight fails
                // it could simply be that the element has become stale.
                let _ = self
                    .driver
                    .execute(
                        r#"
            arguments[0].style.border = 'none';
            "#,
                        vec![curr_elem
                            .to_json()
                            .map_err(|_| self.error("Error jsonifying element"))?],
                    )
                    .await;
            }
        }

        // Set the current element
        self.curr_elem = Some(elem);
        Ok(())
    }

    /// Returns a reference to the current element for performing operations on, or an
    /// error if there is no current element.
    fn get_curr_elem(&self) -> RuntimeResult<&WebElement, String> {
        self.curr_elem
            .as_ref()
            .ok_or(self.error("No element currently located. Try using the locate command"))
    }

    /// Executes a single SchnauzerUI statement.
    pub async fn execute_stmt(&mut self, stmt: Stmt) -> RuntimeResult<(), String> {
        // Add the statement to the list of stmts since the last catch-error stmt was encountered.
        // Used by the try-again command to re-execute on an error.
        self.stmts_since_last_error_handling.push(stmt.clone());

        if !self.had_error {
            // Normal Execution
            match stmt {
                Stmt::Cmd(cs) => self.execute_cmd_stmt(cs).await,
                Stmt::If(is) => self.execute_if_stmt(is).await,
                Stmt::SetVariable(sv) => {
                    self.set_variable(sv);
                    Ok(())
                }
                Stmt::Comment(_) => {
                    // Comments are simply added to the report log, so we just ignore them
                    Ok(())
                }
                Stmt::CatchErr(_) => {
                    // If we hit a catch-error stmt but no error occured, we dont do anything.
                    // Clear statements since last error so try-again command doesnt re-execute the entire script.
                    self.stmts_since_last_error_handling.clear();
                    Ok(())
                }
                Stmt::SetTryAgainFieldToFalse => {
                    // This command was inserted by the interpreter as part of executing try-again.
                    // Reaching this command means the second attempt passed without erroring,
                    // so we go back to normal execution mode.
                    self.tried_again = false;
                    Ok(())
                }
            }
        } else {
            // Syncronizing after an error.
            match stmt {
                Stmt::CatchErr(cs) => {
                    // Execute the commands on the catch-error line.
                    self.execute_cmd_stmt(cs).await?;

                    // Exit error mode and continue normal operation.
                    self.had_error = false;
                    Ok(())
                }
                stmt => {
                    // Read in the rest of the stmts until catch-error for possible re-execution.
                    self.stmts_since_last_error_handling.push(stmt);
                    Ok(())
                }
            }
        }
    }

    /// Sets the value of a variable.
    pub fn set_variable(
        &mut self,
        SetVariableStmt {
            variable_name,
            value,
        }: SetVariableStmt,
    ) {
        self.environment.set_variable(variable_name, value);
    }

    /// Tries to retrieve the value of a variable.
    pub fn get_variable(&self, name: &str) -> RuntimeResult<String, String> {
        self.environment
            .get_variable(name)
            .ok_or(self.error("Variable is not yet defined"))
    }

    /// Takes a cmd_param and tries to resolve it to a string. If it's a user provided String literal, just
    /// returns the value of the string. If it's a variable name, tries to retrieve the variable
    /// from the interpreters environment.
    pub fn resolve(&self, cmd_param: CmdParam) -> RuntimeResult<String, String> {
        match cmd_param {
            CmdParam::String(s) => Ok(s),
            CmdParam::Variable(v) => self.get_variable(&v),
        }
    }

    /// If the provided condition does not fail, executes the following cmd_stmt.
    /// Note: Our grammar does not accomodate nested if statements.
    pub async fn execute_if_stmt(
        &mut self,
        IfStmt {
            condition,
            then_branch,
        }: IfStmt,
    ) -> RuntimeResult<(), String> {
        if self.execute_cmd(condition).await.is_ok() {
            self.execute_cmd_stmt(then_branch).await
        } else {
            Ok(())
        }
    }

    /// Execute each cmd until there are no more combining `and` tokens.
    /// Fail early if one command fails.
    #[async_recursion]
    pub async fn execute_cmd_stmt(&mut self, cs: CmdStmt) -> RuntimeResult<(), String> {
        self.execute_cmd(cs.lhs).await?;
        if let Some((_, rhs)) = cs.rhs {
            self.execute_cmd_stmt(*rhs).await
        } else {
            Ok(())
        }
    }

    pub async fn execute_cmd(&mut self, cmd: Cmd) -> RuntimeResult<(), String> {
        // Adding a default wait of 1 second between commands because it just mimics human timing a lot
        // better. Will add a flag to turn this off.
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

        match cmd {
            Cmd::Locate(locator) => self.locate(locator, true).await,
            Cmd::LocateNoScroll(locator) => self.locate(locator, false).await,
            Cmd::Type(txt) => self.type_into_elem(txt).await,
            Cmd::Click => self.click().await,
            Cmd::Refresh => self.refresh().await,
            Cmd::TryAgain => {
                self.try_again();
                Ok(())
            }
            Cmd::Screenshot => self.screenshot().await,
            Cmd::ReadTo(cp) => self.read_to(cp).await,
            Cmd::Url(url) => self.url_cmd(url).await,
            Cmd::Press(cp) => self.press(cp).await,
            Cmd::Chill(cp) => self.chill(cp).await,
            Cmd::Select(cp) => self.select(cp).await,
            Cmd::DragTo(cp) => self.drag_to(cp).await,
            Cmd::Upload(cp) => self.upload(cp).await,
        }
    }

    pub async fn upload(&mut self, cp: CmdParam) -> RuntimeResult<(), String> {
        // Uploading to a file input is the same as typing keys into it,
        // but our users shouldn't have to know that.

        let path = self.resolve(cp)?;
        self.get_curr_elem()?
            .send_keys(path)
            .await
            .map_err(|_| self.error("Error uploading file"))
    }

    pub async fn drag_to(&mut self, cp: CmdParam) -> RuntimeResult<(), String> {
        let current = self.get_curr_elem()?.clone();
        self.locate(cp, false).await?;
        current.js_drag_to(self.get_curr_elem()?).await.map_err(|_| self.error("Error dragging element."))
    }

    pub async fn select(&mut self, cp: CmdParam) -> RuntimeResult<(), String> {
        let option_text = self.resolve(cp)?;

        // Sometimes, a Select element's only visible text on the page
        // is it's default option. Many users may try to locate
        // the select element based on that text and have to dive into the html
        // before realizing they aren't locating the select element. To prevent
        // this, when select is called, if the currently selected element is an option,
        // we first change it to the parent select containing it.
        if self
            .get_curr_elem()?
            .tag_name()
            .await
            .map_err(|_| self.error("Error getting element tag name"))?
            == "option"
        {
            let parent_select = self
                .get_curr_elem()?
                .query(By::XPath("./.."))
                .first()
                .await
                .map_err(|_| {
                    self.error(
                        "Error getting parent select. Try locating the select element directly",
                    )
                })?;
            self.set_curr_elem(parent_select, false).await?;
        }

        // Try to create a select element from the current located element
        let select_elm = SelectElement::new(self.get_curr_elem()?)
            .await
            .map_err(|_| self.error("Element is not a <select> element"))?;

        // Try to select the element by text
        select_elm
            .select_by_visible_text(&option_text)
            .await
            .map_err(|_| self.error(&format!("Could not select text {}", option_text)))
    }

    pub async fn chill(&mut self, cp: CmdParam) -> RuntimeResult<(), String> {
        let time_to_wait = match self.resolve(cp)?.parse::<u64>() {
            Ok(time) => time,
            _ => return Err(self.error("Could not parse time to wait as integer.")),
        };

        tokio::time::sleep(tokio::time::Duration::from_secs(time_to_wait)).await;

        Ok(())
    }

    pub async fn press(&mut self, cp: CmdParam) -> RuntimeResult<(), String> {
        let key_to_press = match self.resolve(cp)?.as_ref() {
            "Enter" => Key::Enter,
            _ => return Err(self.error("Unsupported Key")),
        };
        self.get_curr_elem()?
            .send_keys("" + key_to_press)
            .await
            .map_err(|_| {
                self.error("Error pressing key. Make sure you have an element in focus first.")
            })
    }

    /// Reads the text of the currently located element to a variable.
    pub async fn read_to(&mut self, name: String) -> RuntimeResult<(), String> {
        let txt = self
            .get_curr_elem()?
            .text()
            .await
            .map_err(|_| self.error("Error getting text from element"))?;
        self.environment.set_variable(name, txt);
        Ok(())
    }

    /// Re-executes the commands since the last catch-error stmt.
    pub fn try_again(&mut self) {
        self.tried_again = true;
        self.stmts.push(Stmt::SetTryAgainFieldToFalse);

        // This would be more efficient with some kind of mem_swap type function.
        self.stmts
            .append(&mut self.stmts_since_last_error_handling.clone());
        self.stmts_since_last_error_handling.clear();
    }

    /// Takes a screenshot of the page.
    pub async fn screenshot(&mut self) -> RuntimeResult<(), String> {
        self.log_cmd(&format!("Taking a screenshot"));
        let ss = self
            .driver
            .screenshot_as_png()
            .await
            .map_err(|_| self.error("Error taking screenshot."))?;
        self.screenshot_buffer.push(ss);
        Ok(())
    }

    /// Refreshes the webpage
    pub async fn refresh(&mut self) -> RuntimeResult<(), String> {
        self.driver
            .refresh()
            .await
            .map_err(|_| self.error("Error refreshing page"))
    }

    /// Tries to click on the currently located web element.
    pub async fn click(&mut self) -> RuntimeResult<(), String> {
        self.driver
            .action_chain()
            .move_to_element_center(self.get_curr_elem()?)
            .click()
            .perform()
            .await
            .map_err(|_| self.error("Error clicking element"))
    }

    /// Tries to type into the current element
    pub async fn type_into_elem(&mut self, cmd_param: CmdParam) -> RuntimeResult<(), String> {
        let txt = self.resolve(cmd_param)?;
        self.get_curr_elem()?
            .clear()
            .await
            .map_err(|_| self.error("Error clearing element"))?;
        self.get_curr_elem()?
            .send_keys(txt)
            .await
            .map_err(|_| self.error("Error typing into element"))
    }

    /// Navigates to the provided url.
    pub async fn url_cmd(&mut self, url: CmdParam) -> RuntimeResult<(), String> {
        let url = self.resolve(url)?;
        self.driver
            .goto(url)
            .await
            .map_err(|_| self.error("Error navigating to page."))
    }

    /// Attempt to locate an element on the page, testing the locator in the following precedence
    /// (placeholder, preceding label, text, id, name, title, class, xpath)
    pub async fn locate(
        &mut self,
        locator: CmdParam,
        scroll_into_view: bool,
    ) -> RuntimeResult<(), String> {
        let locator = self.resolve(locator)?;
        for wait in [0, 5, 10] {
            // Locate an element by its placeholder
            if let Ok(found_elem) = self
                .driver
                .query(By::XPath(&format!("//input[@placeholder='{}']", locator)))
                .wait(Duration::from_secs(wait), Duration::from_secs(1))
                .first()
                .await
            {
                return self.set_curr_elem(found_elem, scroll_into_view).await;
            }

            // Locate an input element by a preceding label
            let label_locator = format!("//label[text()='{}']/../input", locator);
            if let Ok(found_elem) = self
                .driver
                .query(By::XPath(&label_locator))
                .nowait()
                .first()
                .await
            {
                return self.set_curr_elem(found_elem, scroll_into_view).await;
            }

            // Try to find the element by its text
            if let Ok(found_elem) = self
                .driver
                .query(By::XPath(&format!("//*[text()='{}']", locator)))
                .nowait()
                .first()
                .await
            {
                return self.set_curr_elem(found_elem, scroll_into_view).await;
            }

            // Try to find the element by partial text
            if let Ok(found_elem) = self
                .driver
                .query(By::XPath(&format!("//*[contains(text(), '{}')]", locator)))
                .nowait()
                .first()
                .await
            {
                return self.set_curr_elem(found_elem, scroll_into_view).await;
            }

            // Try to find an element by it's title
            if let Ok(found_elem) = self
                .driver
                .query(By::XPath(&format!("//*[@title='{}']", locator)))
                .nowait()
                .first()
                .await
            {
                return self.set_curr_elem(found_elem, scroll_into_view).await;
            }

            // Try to find an element by it's id
            if let Ok(found_elem) = self.driver.query(By::Id(&locator)).nowait().first().await {
                return self.set_curr_elem(found_elem, scroll_into_view).await;
            }

            // Try to find an element by it's name
            if let Ok(found_elem) = self.driver.query(By::Name(&locator)).nowait().first().await {
                return self.set_curr_elem(found_elem, scroll_into_view).await;
            }

            // Try to find an element by it's class
            if let Ok(found_elem) = self
                .driver
                .query(By::ClassName(&locator))
                .nowait()
                .first()
                .await
            {
                return self.set_curr_elem(found_elem, scroll_into_view).await;
            }

            // Try to find an element by xpath
            if let Ok(found_elem) = self
                .driver
                .query(By::XPath(&locator))
                .nowait()
                .first()
                .await
            {
                return self.set_curr_elem(found_elem, scroll_into_view).await;
            }
        }

        Err(self.error("Could not locate the element"))
    }
}
