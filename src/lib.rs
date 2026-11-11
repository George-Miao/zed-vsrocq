//! Launches the native VsRocq bridge as Zed's Rocq language server.

mod bridge;
mod config;
mod error;
mod server;

use config::Config;
use serde_json::Value;
use server::{Launch, Launches};
use snafu::ResultExt;
use vsrocq_settings::Settings;
use zed_extension_api::{self as zed, LanguageServerId, settings::LspSettings};

use crate::error::Result;

struct VsRocq {
    bridge: bridge::Bridge,
    launches: Launches,
}

impl VsRocq {
    fn launch(&mut self, settings: &Config, worktree: &zed::Worktree) -> Result<Launch> {
        if let Some((config, launch)) = self.launches.get(&worktree.id()) {
            if config == &settings.server {
                return Ok(launch.clone());
            }
        }
        let env = server::environment(worktree, settings.binary.as_ref());
        let launch = server::discover(&settings.server, worktree, &env)?;
        self.launches
            .insert(worktree.id(), (settings.server.clone(), launch.clone()));
        Ok(launch)
    }
}

fn settings(worktree: &zed::Worktree) -> Result<Config> {
    let raw = error::host(LspSettings::for_worktree("vsrocq", worktree)).context(error::ReadSettingsSnafu)?;
    Config::new(
        decode_settings(raw.initialization_options, "initialization_options")?,
        decode_settings(raw.settings, "settings")?,
        raw.binary,
    )
}

// Zed owns Value in this API. Decode it immediately; application settings
// retain typed fields and opaque raw metadata instead.
fn decode_settings(value: Option<Value>, name: &'static str) -> Result<Settings> {
    let value = match value {
        None | Some(Value::Null) => return Ok(Settings::default()),
        Some(value) => value,
    };
    serde_json::from_value::<Settings>(value).context(error::DecodeConfigurationSnafu { section: name })
}

impl zed::Extension for VsRocq {
    fn new() -> Self {
        Self {
            bridge: bridge::Bridge::default(),
            launches: Launches::default(),
        }
    }

    fn language_server_command(
        &mut self,
        language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<zed::Command> {
        let settings = settings(worktree)?;
        let env = server::environment(worktree, settings.binary.as_ref());
        // Refresh shell and opam discovery whenever Zed restarts this
        // worktree's server.
        let launch = server::discover(&settings.server, worktree, &env)?;
        self.launches
            .insert(worktree.id(), (settings.server.clone(), launch.clone()));
        let command = self
            .bridge
            .resolve(language_server_id, &settings, worktree)
            .inspect_err(|err| {
                zed::set_language_server_installation_status(
                    language_server_id,
                    &zed::LanguageServerInstallationStatus::Failed(err.report()),
                );
            })?;
        let mut args = settings
            .binary
            .as_ref()
            .and_then(|binary| binary.arguments.clone())
            .unwrap_or_default();
        args.extend(["--server".into(), launch.path]);
        args.extend(launch.args.into_iter().map(|arg| format!("--server-arg={arg}")));
        args.extend(["--root".into(), worktree.root_path()]);
        Ok(zed::Command { command, args, env })
    }

    fn language_server_initialization_options(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<Option<Value>> {
        let settings = settings(worktree)?;
        let launch = self.launch(&settings, worktree)?;
        serde_json::to_value(settings.resolved(&launch.path, &launch.args))
            .map(Some)
            .context(error::EncodeConfigurationSnafu {
                section: "initialization options",
            })
            .map_err(String::from)
    }

    fn language_server_workspace_configuration(
        &mut self,
        _language_server_id: &LanguageServerId,
        worktree: &zed::Worktree,
    ) -> zed::Result<Option<Value>> {
        let settings = settings(worktree)?;
        let launch = self.launch(&settings, worktree)?;
        serde_json::to_value(settings.resolved(&launch.path, &launch.args))
            .map(Some)
            .context(error::EncodeConfigurationSnafu {
                section: "workspace settings",
            })
            .map_err(String::from)
    }
}

zed::register_extension!(VsRocq);
