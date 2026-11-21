//! Errors that carry a stable code and exit status, so the `/clip` skill can
//! react to them without parsing messages.
//!
//! Exit codes:
//! - 0: success
//! - 1: any other error
//! - 2: invalid command line (raised by clap)
//! - 3: the command exists but is not implemented yet
//! - 4: setup is incomplete (`jc doctor` found a problem)
//! - 5: project not found

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Other,
    NotImplemented,
    SetupIncomplete,
    ProjectNotFound,
}

impl ErrorKind {
    pub fn code(self) -> &'static str {
        match self {
            ErrorKind::Other => "error",
            ErrorKind::NotImplemented => "not_implemented",
            ErrorKind::SetupIncomplete => "setup_incomplete",
            ErrorKind::ProjectNotFound => "project_not_found",
        }
    }

    pub fn exit_code(self) -> u8 {
        match self {
            ErrorKind::Other => 1,
            ErrorKind::NotImplemented => 3,
            ErrorKind::SetupIncomplete => 4,
            ErrorKind::ProjectNotFound => 5,
        }
    }
}

/// An error with a machine-readable kind and an optional hint for the user.
#[derive(Debug)]
pub struct JcError {
    pub kind: ErrorKind,
    pub message: String,
    pub hint: Option<String>,
}

impl JcError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn not_implemented(command: &str, milestone: &str) -> Self {
        Self::new(
            ErrorKind::NotImplemented,
            format!("`jc {command}` is not implemented yet (planned for {milestone})"),
        )
    }
}

impl fmt::Display for JcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for JcError {}

/// Finds the [`JcError`] in an error chain, if there is one.
pub fn classify(err: &anyhow::Error) -> (ErrorKind, Option<&str>) {
    match err.chain().find_map(|e| e.downcast_ref::<JcError>()) {
        Some(e) => (e.kind, e.hint.as_deref()),
        None => (ErrorKind::Other, None),
    }
}
