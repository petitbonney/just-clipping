//! How commands report results.
//!
//! stdout carries the result only: JSON with `--json`, readable text
//! otherwise. Progress and log lines always go to stderr, so a caller can
//! parse stdout without filtering.

use crate::error;
use serde::Serialize;

/// A command's result, printable as JSON or as text.
pub trait Report {
    fn to_json(&self) -> serde_json::Value;
    fn to_human(&self, quiet: bool) -> String;
    /// Non-zero when the command ran but found a problem (for example
    /// `jc doctor` with a missing tool).
    fn exit_code(&self) -> u8 {
        0
    }
}

/// Converts any serializable value into the JSON side of a [`Report`].
pub fn json_of(value: &impl Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("report types always serialize")
}

pub fn print_report(report: &dyn Report, json: bool, quiet: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report.to_json()).expect("valid JSON value")
        );
    } else {
        let text = report.to_human(quiet);
        if !text.is_empty() {
            print!("{text}");
            if !text.ends_with('\n') {
                println!();
            }
        }
    }
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<&'a str>,
}

/// Prints an error and returns the process exit code for it.
pub fn print_error(err: &anyhow::Error, json: bool) -> u8 {
    let (kind, hint) = error::classify(err);
    // Include the context chain ("reading project.json: invalid JSON …").
    let message = format!("{err:#}");
    if json {
        let body = serde_json::json!({ "error": ErrorBody { code: kind.code(), message, hint } });
        println!(
            "{}",
            serde_json::to_string_pretty(&body).expect("valid JSON value")
        );
    } else {
        eprintln!("error: {message}");
        if let Some(hint) = hint {
            eprintln!("  hint: {hint}");
        }
    }
    kind.exit_code()
}

/// Logs a progress line to stderr, which never mixes with stdout results.
pub fn log(msg: impl AsRef<str>) {
    eprintln!("{}", msg.as_ref());
}
