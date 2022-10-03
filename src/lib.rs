//! SchnauzerUI is a human readable DSL for performing automated UI testing in the browser.
//! The main goal of SchnauzerUI is to increase stakeholder visibility and participation in 
//! Quality Assurance testing. Rather than providing a shim to underling code written by 
//! a QA engineer (see [Cucumber](https://cucumber.io/)), SchnauzerUI is the only source of truth for a 
//! test's execution. In this way, SchnauzerUI aims to provide a test report you can trust.
//! 
//! SchnauzerUI is under active development, the progress of which is being recorded as a 
//! youtube video series: https://www.youtube.com/playlist?list=PLK0mRy_gymKMLPlQ-ZAYfpBzXWjK7W9ER.
//! 
//! # Installation
//! 
//! SchnauzerUI is a binary, but we are not doing pre-built binaries at this time.
//! To install it, make sure you have Cargo and Rust installed on your system,
//! then run `cargo install --git https://github.com/bcpeinhardt/schnauzerUI`.
//! 
//! SchnauzerUI is __also__ a Rust library, which can be used in other projects. 
//! 
//! # Language
//! 
//! Let's look at an example:
//! ```SchnauzerUI
//! # Type in username (located by labels)
//! locate "Username" and type "test@test.com"
//! 
//! # Type in password (located by placeholder)
//! locate "Password" and type "Password123!"
//! 
//! # Click the submit button (located by element text)
//! locate "Submit" and click 
//! ```
//! 
//! A SchnauzerUI script is composed of commands that execute on top of running Selenium webdrivers.
//! 
//! A `#` creates a comment. Comments in SchnauzerUI are automatically added to test reports.
//! 
//! The `locate` command locates a WebElement in the most straightforward way possible. It begins with 
//! aspects of the element that are __visible to the user__ (placeholder, adjacent label, text). This is important for a few reasons:
//! 
//! 1. QA testers rarely need to go digging around in HTML to write tests.
//! 2. Tests are more likely to survive a change in technology (for example, migrating JavaScript frameworks).
//! 3. Tests are more representative of user experience (The user doesn't care about test_ids, they do care about placeholders).
//! 
//! Then, the `locate` command can default to more technology specific locators, in order to allow flexibility in 
//! test authoring (id, name, title, class, xpath)
//! 
//! Once an element is in focus (i.e. located), any subsequent commands will be executed against it. Commands relating
//! to web elements include `click`, `type`, and `read-to` (a command for storing the text of a web element as a variable).
//! 
//! SchnauzerUI also includes a concept of error handling. UI tests can be brittle. Sometimes you simply want to write a long
//! test flow (even when testing gurus tell you not too) without it bailing at the first slow page load. For this, SchnauzerUI
//! provides the `catch-error:` command for gracefully recovering from errors and resuming test runs. We can improve the 
//! previous test example like so
//! ```SchnauzerUI
//! # Type in username (located by labels)
//! locate "Username" and type "test@test.com"
//! 
//! # Type in password (located by placeholder)
//! locate "Password" and type "Password123!"
//! 
//! # Click the submit button (located by element text)
//! locate "Submit" and click 
//! 
//! # This page is quite slow to load, so we'll try again if something goes wrong
//! catch-error: screenshot and refresh and try-again
//! 
//! ................
//! ```
//! 
//! Here, the `catch-error:` command gives us the chance to reset the page by refreshing
//! and try the previous commands again without the test simply failing. The test "failure"
//! is still reported (and a screenshot is taken), but the rest of the test executes.
//! 
//! (Note: This does not risk getting caught in a loop. The `try-again` command will only re-execute
//! the same code once.)
//! 
//! # Running the tests
//! Before running the tests you will need firefox and geckodriver installed and in your path.
//! Then
//! 
//! 1. Start selenium. SchnauzerUI is executed against a standalone selenium grid (support for configuring
//! SchnauzerUI to run against an existing selenium infrastructure is on the todo list). To run the provided 
//! selenium grid, `cd` into the selenium directory in a new terminal and run
//! ```bash
//! java -jar .\selenium-server-<version>.jar standalone
//! ```
//! It should default to port 4444.
//! 
//! 2. The tests come with accompanying HTML files. The easiest way to serve the files to localhost 
//! is probably to use python. In another new terminal, run the command
//! ```python
//! python -m http.server 1234
//! ```
//! 
//! From there, it should be a simple `cargo test`. The tests will take a moment to execute,
//! as they will launch browsers to run in. 

pub mod interpreter;
pub mod parser;
pub mod scanner;
pub mod environment;

use interpreter::Interpreter;
use parser::Parser;
use scanner::{Scanner, Token};
use thirtyfour::prelude::WebDriverResult;

pub async fn run(code: String) -> WebDriverResult<bool> {
    let mut scanner = Scanner::from_src(code);
    let tokens = scanner.scan();

    let stmts = Parser::new().parse(tokens);

    let mut interpreter = Interpreter::new(stmts).await?;
    interpreter.interpret().await
}
