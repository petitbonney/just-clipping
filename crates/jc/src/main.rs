use clap::Parser;
use jc::cli::{Cli, Command};
use jc::output;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    let quiet = matches!(&cli.command, Command::Doctor(args) if args.quiet);
    match jc::run(cli) {
        Ok(report) => {
            output::print_report(report.as_ref(), json, quiet);
            ExitCode::from(report.exit_code())
        }
        Err(err) => ExitCode::from(output::print_error(&err, json)),
    }
}
