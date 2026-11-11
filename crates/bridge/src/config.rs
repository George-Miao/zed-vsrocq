use std::ffi::OsString;

use serde_json::value::RawValue;
use snafu::{ResultExt, Snafu};
use vsrocq_settings::{ServerSettings, Settings};

use crate::cli::Options;

#[derive(Debug, Snafu)]
pub(crate) enum Error {
    #[snafu(display("cannot parse VsRocq settings: {source}"))]
    ParseSettings { source: vsrocq_settings::Error },

    #[snafu(display("vsrocq.server.path must be a string"))]
    NullServerPath,

    #[snafu(display("cannot normalize VsRocq server settings: {source}"))]
    NormalizeSettings { source: vsrocq_settings::Error },
}

pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) const SETUP_URL: &str = "https://github.com/rocq-prover/vsrocq#installation";

#[derive(Clone, Debug)]
pub(crate) struct Config {
    pub(crate) server: OsString,
    pub(crate) args: Vec<OsString>,
    pub(crate) auto_open: bool,
    settings: ServerSettings,
}

impl Config {
    pub(crate) fn new(options: &Options) -> Self {
        Self {
            server: options.server.clone(),
            args: options.args.clone(),
            auto_open: true,
            settings: ServerSettings::default(),
        }
    }

    pub(crate) fn update(&mut self, value: &RawValue) -> Result<()> {
        let settings = if value.get().trim() == "null" {
            Settings::default()
        } else {
            Settings::parse(value).context(ParseSettingsSnafu)?
        };
        let command = settings.server.get();
        let server = match command.and_then(|server| server.path.get()) {
            Some(Some(path)) => Some(OsString::from(path.as_str())),
            Some(None) => return NullServerPathSnafu.fail(),
            None => None,
        };
        let args = command
            .and_then(|server| server.args.get())
            .map(|args| args.iter().map(|arg| OsString::from(arg.as_str())).collect());
        let auto_open = settings
            .proof
            .get()
            .and_then(|proof| proof.auto_open.get())
            .copied()
            .unwrap_or(true);
        let settings = ServerSettings::from_settings(settings).context(NormalizeSettingsSnafu)?;
        if let Some(server) = server {
            self.server = server;
        }
        if let Some(args) = args {
            self.args = args;
        }
        self.auto_open = auto_open;
        self.settings = settings;
        Ok(())
    }

    pub(crate) fn server_settings(&self) -> ServerSettings {
        self.settings.clone()
    }

    pub(crate) fn proof_settings(&self) -> ServerSettings {
        self.settings.for_proof()
    }

    pub(crate) fn same_command(&self, other: &Self) -> bool {
        self.server == other.server && self.args == other.args
    }
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsString, path::PathBuf};

    use serde::Deserialize;

    use super::{Config, Error};
    use crate::cli::Options;

    fn config() -> Config {
        Config::new(&Options {
            root: PathBuf::from("."),
            server: OsString::from("vsrocqtop"),
            args: Vec::new(),
        })
    }

    fn raw(text: &str) -> Box<serde_json::value::RawValue> {
        serde_json::from_str(text).unwrap()
    }

    #[derive(Deserialize)]
    struct Forwarded {
        proof: Proof,
        completion: Completion,
        diagnostics: Diagnostics,
        server: Option<Box<serde_json::value::RawValue>>,
        bridge: Option<Box<serde_json::value::RawValue>>,
        unknown: Option<Box<serde_json::value::RawValue>>,
    }

    #[derive(Deserialize)]
    struct Proof {
        block: bool,
        mode: u8,
        delegation: String,
        workers: Option<i64>,
        #[serde(rename = "autoOpen")]
        auto_open: Option<bool>,
        #[serde(rename = "pointInterpretationMode")]
        point_interpretation_mode: u8,
        future: Option<Future>,
    }

    #[derive(Deserialize)]
    struct Future {
        flag: bool,
    }

    #[derive(Deserialize)]
    struct Completion {
        enable: bool,
        #[serde(rename = "atomicFactor")]
        atomic_factor: Option<f64>,
        #[serde(rename = "sizeFactor")]
        size_factor: Option<f64>,
    }

    #[derive(Deserialize)]
    struct Diagnostics {
        enable: bool,
        full: bool,
    }

    fn forwarded(settings: &vsrocq_settings::ServerSettings) -> Forwarded {
        serde_json::from_str(&serde_json::to_string(settings).unwrap()).unwrap()
    }

    #[test]
    fn adds_required_server_defaults_and_keeps_bridge_settings_local() {
        let mut config = config();
        config
            .update(&raw(r#"{
                "server":{"path":"opam","args":["exec","--","vsrocqtop"]},
                "bridge":{"path":"local"},
                "proof":{"block":false,"autoOpen":false,"mode":1,"pointInterpretationMode":1,
                    "future":{"flag":true}},
                "completion":{"atomicFactor":2.5,"sizeFactor":3.5},
                "diagnostics":{"enable":false,"full":false},
                "unknown":{"keep":"extension-only"}
            }"#))
            .unwrap();
        let settings = forwarded(&config.server_settings());
        assert!(!settings.proof.block);
        assert_eq!(settings.proof.mode, 0);
        assert_eq!(settings.proof.point_interpretation_mode, 0);
        assert_eq!(settings.proof.delegation, "None");
        assert_eq!(settings.proof.workers, None);
        assert!(settings.proof.auto_open.is_none());
        assert!(settings.proof.future.unwrap().flag);
        assert!(!config.auto_open);
        assert!(settings.completion.enable);
        assert_eq!(settings.completion.atomic_factor, Some(2.5));
        assert_eq!(settings.completion.size_factor, Some(3.5));
        assert!(!settings.diagnostics.enable);
        assert!(!settings.diagnostics.full);
        assert!(settings.server.is_none());
        assert!(settings.bridge.is_none());
        assert!(settings.unknown.is_none());
        let private = forwarded(&config.proof_settings());
        assert!(private.diagnostics.enable);
        assert!(private.diagnostics.full);
    }

    #[test]
    fn resets_server_settings_but_retains_the_resolved_command_when_omitted() {
        let mut config = config();
        config
            .update(&raw(r#"{"server":{"path":"opam","args":["exec","--","vsrocqtop"]},
                "proof":{"block":false,"autoOpen":false}}"#))
            .unwrap();
        config.update(&raw("{}")).unwrap();
        assert_eq!(config.server, OsString::from("opam"));
        assert_eq!(config.args, ["exec", "--", "vsrocqtop"].map(OsString::from));
        assert!(config.auto_open);
        let settings = forwarded(&config.server_settings());
        assert!(settings.proof.block);
        assert_eq!(settings.completion.atomic_factor, None);
        assert_eq!(settings.completion.size_factor, None);
    }

    #[test]
    fn does_not_select_an_unknown_setting_named_after_the_language_server() {
        let mut config = config();
        config
            .update(&raw(r#"{"vsrocq":{
                "server":{"path":"wrong-server","args":["wrong-arg"]},
                "proof":{"block":false,"autoOpen":false}
            }}"#))
            .unwrap();
        assert_eq!(config.server, OsString::from("vsrocqtop"));
        assert!(config.args.is_empty());
        assert!(config.auto_open);
        assert!(forwarded(&config.server_settings()).proof.block);
    }

    #[test]
    fn rejects_null_nonnullable_settings_without_changing_the_command() {
        let mut config = config();
        for (text, field) in [
            (r#"{"server":{"path":null}}"#, None),
            (r#"{"server":{"args":null}}"#, Some("vsrocq.server.args")),
            (r#"{"proof":{"block":null}}"#, Some("vsrocq.proof.block")),
            (r#"{"proof":{"autoOpen":null}}"#, Some("vsrocq.proof.autoOpen")),
            (r#"{"goals":null}"#, Some("vsrocq.goals")),
        ] {
            match (config.update(&raw(text)).unwrap_err(), field) {
                (Error::NullServerPath, None) => {}
                (
                    Error::NormalizeSettings {
                        source: vsrocq_settings::Error::InvalidSetting { field, .. },
                    },
                    Some(expected),
                ) => assert_eq!(field, expected),
                error => panic!("unexpected settings failure: {error:?}"),
            }
            assert_eq!(config.server, OsString::from("vsrocqtop"));
        }
        config.update(&raw(r#"{"proof":{"workers":null}}"#)).unwrap();
    }
}
