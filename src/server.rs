use std::collections::HashMap;

use snafu::ensure;
use zed_extension_api::{self as zed, Architecture, Os};

use crate::{
    config::Server,
    error::{self, Result},
};

#[derive(Clone, Debug)]
pub(crate) struct Launch {
    pub(crate) path: String,
    pub(crate) args: Vec<String>,
}

pub(crate) fn discover(config: &Server, worktree: &zed::Worktree, env: &zed::EnvVars) -> Result<Launch> {
    if let Some(path) = &config.path {
        return Ok(Launch {
            path: configured_path(path, worktree)?,
            args: config.args.clone(),
        });
    }
    if let Some(path) = worktree.which("vsrocqtop") {
        return Ok(Launch {
            path,
            args: config.args.clone(),
        });
    }
    if let Some(opam) = worktree.which("opam") {
        let root = worktree.root_path();
        if let Some(bin) = opam_bin(&opam, Some(&root), env) {
            return Ok(opam_launch(opam, Some(root), &bin, config));
        }
        if let Some(bin) = opam_bin(&opam, None, env) {
            return Ok(opam_launch(opam, None, &bin, config));
        }
    }
    // Keep the bridge alive to report a missing toolchain and offer guided
    // setup in the proof view.
    Ok(Launch {
        path: "vsrocqtop".into(),
        args: config.args.clone(),
    })
}

pub(crate) fn environment(worktree: &zed::Worktree, binary: Option<&zed::settings::CommandSettings>) -> zed::EnvVars {
    let mut env = worktree.shell_env();
    let windows = matches!(zed::current_platform().0, Os::Windows);
    if let Some(overrides) = binary.and_then(|binary| binary.env.as_ref()) {
        for (key, value) in overrides {
            if let Some((_, old)) = env.iter_mut().find(|(name, _)| {
                if windows {
                    name.eq_ignore_ascii_case(key)
                } else {
                    name == key
                }
            }) {
                old.clone_from(value);
            } else {
                env.push((key.clone(), value.clone()));
            }
        }
    }
    env
}

pub(crate) fn configured_path(path: &str, worktree: &zed::Worktree) -> Result<String> {
    ensure!(!path.trim().is_empty(), error::EmptyExecutableSnafu);
    if absolute_path(path) {
        return Ok(path.into());
    }
    if path.contains('/') || path.contains('\\') {
        let root = worktree.root_path();
        return Ok(format!("{root}/{path}"));
    }
    Ok(worktree.which(path).unwrap_or_else(|| path.into()))
}

fn absolute_path(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with("\\\\")
        || (path.as_bytes().get(1) == Some(&b':') && matches!(path.as_bytes().get(2), Some(b'/' | b'\\')))
}

fn opam_bin(opam: &str, switch: Option<&str>, env: &zed::EnvVars) -> Option<String> {
    let mut command = zed::process::Command::new(opam)
        .args(["var", "bin"])
        .envs(env.iter().cloned());
    if let Some(switch) = switch {
        command = command.arg("--switch").arg(switch);
    }
    let output = command.output().ok()?;
    if output.status != Some(0) {
        return None;
    }
    let bin = String::from_utf8(output.stdout).ok()?;
    let bin = bin.trim();
    if bin.is_empty() || !absolute_path(bin) {
        return None;
    }
    Some(bin.into())
}

fn opam_launch(opam: String, switch: Option<String>, bin: &str, config: &Server) -> Launch {
    let mut args = vec!["exec".into()];
    if let Some(switch) = switch {
        args.extend(["--switch".into(), switch]);
    }
    let suffix = if matches!(zed::current_platform().0, Os::Windows) {
        ".exe"
    } else {
        ""
    };
    args.extend(["--".into(), format!("{bin}/vsrocqtop{suffix}")]);
    args.extend(config.args.iter().cloned());
    Launch { path: opam, args }
}

pub(crate) fn target(os: Os, architecture: Architecture) -> Result<&'static str> {
    match (os, architecture) {
        (Os::Linux, Architecture::X8664) => Ok("x86_64-unknown-linux-gnu"),
        (Os::Windows, Architecture::X8664) => Ok("x86_64-pc-windows-msvc"),
        (Os::Mac, Architecture::X8664) => Ok("x86_64-apple-darwin"),
        (Os::Mac, Architecture::Aarch64) => Ok("aarch64-apple-darwin"),
        _ => error::PlatformSnafu { os, architecture }.fail(),
    }
}

pub(crate) type Launches = HashMap<u64, (Server, Launch)>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_native_absolute_paths() {
        for path in [
            "/tmp/server",
            "C:\\tools\\server.exe",
            "D:/tools/server.exe",
            "\\\\host\\share\\server.exe",
        ] {
            assert!(absolute_path(path), "{path}");
        }
        for path in ["server", "target/debug/server", "C:server.exe"] {
            assert!(!absolute_path(path), "{path}");
        }
    }
}
