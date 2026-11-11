use std::{ffi::OsString, io, path::PathBuf};

use clap::Parser;
use snafu::{ResultExt, Snafu, ensure};

#[derive(Debug, Snafu)]
pub(crate) enum Error {
    #[snafu(display("cannot read the current project directory"))]
    CurrentDirectory { source: io::Error },

    #[snafu(display("cannot resolve project root {}", path.display()))]
    ResolveRoot { path: PathBuf, source: io::Error },

    #[snafu(display("project root {} is not a directory", path.display()))]
    RootNotDirectory { path: PathBuf },
}

pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug)]
pub(crate) struct Options {
    pub(crate) root: PathBuf,
    pub(crate) server: OsString,
    pub(crate) args: Vec<OsString>,
}

#[derive(Parser)]
#[command(name = "vsrocq-bridge", about = "VsRocq LSP proxy and proof view")]
struct Arguments {
    #[arg(long, default_value = "vsrocqtop")]
    server: OsString,
    #[arg(long = "server-arg", allow_hyphen_values = true)]
    args: Vec<OsString>,
    #[arg(long)]
    root: Option<PathBuf>,
}

pub(crate) fn parse() -> Result<Options> {
    let arguments = Arguments::parse();
    let root = match arguments.root {
        Some(root) => root,
        None => std::env::current_dir().context(CurrentDirectorySnafu)?,
    };
    Ok(Options {
        root: resolve_root(root)?,
        server: arguments.server,
        args: arguments.args,
    })
}

fn resolve_root(root: PathBuf) -> Result<PathBuf> {
    let root = root.canonicalize().context(ResolveRootSnafu { path: root.as_path() })?;
    ensure!(root.is_dir(), RootNotDirectorySnafu { path: root });
    Ok(root)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::Arguments;

    #[test]
    fn accepts_repeated_server_arguments_including_leading_hyphens() {
        let arguments = Arguments::try_parse_from([
            "vsrocq-bridge",
            "--server",
            "opam",
            "--server-arg=exec",
            "--server-arg=--switch",
            "--server-arg=--",
            "--server-arg",
            "-without-project-file",
            "--root",
            ".",
        ])
        .unwrap();
        assert_eq!(arguments.args, ["exec", "--switch", "--", "-without-project-file"]);
        assert_eq!(arguments.server, "opam");
    }

    #[test]
    fn rejects_task_client_subcommands() {
        assert!(Arguments::try_parse_from(["vsrocq-bridge", "command", "step-forward"]).is_err());
    }
}
