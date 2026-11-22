//! `jc`: the deterministic half of just-clipping. It downloads, analyzes,
//! cuts and renders; the `/clip` skill in Claude Code makes the judgment
//! calls. The two talk through JSON files in the project folder, and `jc`
//! never calls an LLM itself.

pub mod analyze;
pub mod chat;
pub mod cli;
pub mod config;
pub mod data;
pub mod doctor;
pub mod error;
pub mod ingest;
pub mod output;
pub mod paths;
pub mod process;
pub mod project;
pub mod status;
pub mod tools;

use anyhow::Result;
use cli::{Cli, Command};
use error::JcError;
use output::Report;

/// Runs a parsed command line and returns its report.
pub fn run(cli: Cli) -> Result<Box<dyn Report>> {
    let paths = paths::Paths::resolve(cli.workspace)?;
    match cli.command {
        Command::Doctor(args) => {
            let config = config::Config::load(&paths.config_file())?;
            Ok(Box::new(doctor::run(&paths, &config, &args)?))
        }
        Command::Status(args) => status::run(&paths, args.project.as_deref()),
        Command::Ingest(args) => {
            let config = config::Config::load(&paths.config_file())?;
            Ok(Box::new(ingest::run(&paths, &config, &args)?))
        }
        Command::Analyze(args) => {
            let config = config::Config::load(&paths.config_file())?;
            Ok(Box::new(analyze::run(&paths, &config, &args)?))
        }
        Command::Candidates(_) => Err(JcError::not_implemented("candidates", "M2").into()),
        Command::Review(_) => Err(JcError::not_implemented("review", "M2").into()),
        Command::Render(_) => Err(JcError::not_implemented("render", "M3").into()),
        Command::Package(_) => Err(JcError::not_implemented("package", "M4").into()),
        Command::Export(_) => Err(JcError::not_implemented("export", "M4").into()),
    }
}
