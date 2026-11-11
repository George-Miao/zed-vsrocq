mod app;
mod cli;
mod config;
mod document;
mod error;
mod http;
mod navigation;
mod process;
mod protocol;
mod secure;
mod wire;

use std::process::ExitCode;

pub(crate) use error::Error;
use snafu::ResultExt;

fn run() -> Result<()> {
    let options = cli::parse()?;
    let runtime = compio::runtime::Runtime::new().context(error::StartRuntimeSnafu)?;
    runtime.block_on(app::run(options))
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("vsrocq-bridge: {}", snafu::Report::from_error(error));
            ExitCode::FAILURE
        }
    }
}

pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;
