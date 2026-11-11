use serde::Serialize;
use snafu::{OptionExt, ResultExt, ensure};
use vsrocq_settings::Settings;
use zed_extension_api::settings::CommandSettings;

use crate::error::{self, Result};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Server {
    pub(crate) path: Option<String>,
    pub(crate) args: Vec<String>,
}

#[derive(Debug, Default)]
pub(crate) struct Bridge {
    pub(crate) path: Option<String>,
    pub(crate) release_repository: Option<String>,
    pub(crate) release_tag: Option<String>,
    pub(crate) download_url: Option<String>,
}

pub(crate) struct Config {
    pub(crate) server: Server,
    pub(crate) bridge: Bridge,
    pub(crate) binary: Option<CommandSettings>,
    settings: Settings,
}

impl Config {
    pub(crate) fn new(initialization: Settings, workspace: Settings, binary: Option<CommandSettings>) -> Result<Self> {
        let mut settings = Settings::extension_defaults();
        settings
            .merge(initialization)
            .and_then(|()| settings.merge(workspace))
            .and_then(|()| settings.validate())
            .context(error::SettingsSnafu)?;
        let proof = settings.proof.get().context(error::ProofSectionSnafu)?;
        ensure!(proof.block.get().is_some(), error::ProofSettingSnafu {
            setting: "block"
        });
        ensure!(proof.auto_open.get().is_some(), error::ProofSettingSnafu {
            setting: "autoOpen"
        });
        let server = std::mem::take(&mut settings.server)
            .into_value()
            .context(error::SettingsSnafu)?
            .unwrap_or_default();
        let bridge = std::mem::take(&mut settings.bridge)
            .into_value()
            .context(error::SettingsSnafu)?
            .unwrap_or_default();
        Ok(Self {
            server: Server {
                path: server.path.into_value().context(error::SettingsSnafu)?.flatten(),
                args: server
                    .args
                    .into_value()
                    .context(error::SettingsSnafu)?
                    .unwrap_or_default(),
            },
            bridge: Bridge {
                path: bridge.path.into_value().context(error::SettingsSnafu)?.flatten(),
                release_repository: bridge
                    .release_repository
                    .into_value()
                    .context(error::SettingsSnafu)?
                    .flatten(),
                release_tag: bridge.release_tag.into_value().context(error::SettingsSnafu)?.flatten(),
                download_url: bridge
                    .download_url
                    .into_value()
                    .context(error::SettingsSnafu)?
                    .flatten(),
            },
            binary,
            settings,
        })
    }

    pub(crate) fn resolved<'a>(&'a self, path: &'a str, args: &'a [String]) -> impl Serialize + 'a {
        self.settings.resolved(path, args)
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde_json::value::RawValue;

    use super::*;

    fn input(text: &str) -> Settings {
        crate::decode_settings(Some(serde_json::from_str(text).unwrap()), "settings").unwrap()
    }

    fn configured(initialization: &str, workspace: &str) -> Config {
        Config::new(input(initialization), input(workspace), None).unwrap()
    }

    #[derive(Debug, Deserialize)]
    struct Forwarded {
        foreign: Option<Foreign>,
        server: Resolved,
        proof: Proof,
        bridge: Option<Box<RawValue>>,
        future: Option<Future>,
        vsrocq: Option<Box<RawValue>>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Resolved {
        path: String,
        args: Vec<String>,
    }

    #[derive(Debug, Deserialize)]
    struct Proof {
        block: bool,
        #[serde(rename = "autoOpen")]
        auto_open: bool,
        future: Option<Future>,
    }

    #[derive(Debug, Deserialize)]
    struct Foreign {
        value: Vec<String>,
    }

    #[derive(Debug, Deserialize)]
    struct Future {
        a: Option<i64>,
        b: Option<i64>,
        nested: Option<Nested>,
    }

    #[derive(Debug, Deserialize)]
    struct Nested {
        first: bool,
        second: bool,
    }

    fn forwarded(value: impl Serialize) -> Forwarded {
        serde_json::from_str(&serde_json::to_string(&value).unwrap()).unwrap()
    }

    #[test]
    fn resolves_proof_defaults_with_workspace_precedence() {
        let settings = configured(
            r#"{"proof":{"block":false,"autoOpen":false}}"#,
            r#"{"proof":{"block":true}}"#,
        );
        let output = forwarded(settings.resolved("vsrocqtop", &[]));
        assert!(output.proof.block);
        assert!(!output.proof.auto_open);
        assert!(output.vsrocq.is_none());

        let settings = configured(r#"{"proof":{"block":false}}"#, "{}");
        let output = forwarded(settings.resolved("vsrocqtop", &[]));
        assert!(!output.proof.block);
        assert!(output.proof.auto_open);

        let settings = configured(
            r#"{"proof":{"block":null,"autoOpen":"invalid"}}"#,
            r#"{"proof":{"block":false,"autoOpen":false}}"#,
        );
        let output = forwarded(settings.resolved("vsrocqtop", &[]));
        assert!(!output.proof.block);
        assert!(!output.proof.auto_open);
    }

    #[test]
    fn missing_and_null_zed_settings_use_extension_defaults() {
        for value in [None, Some(serde_json::Value::Null)] {
            let initialization = crate::decode_settings(value.clone(), "initialization_options").unwrap();
            let workspace = crate::decode_settings(value, "settings").unwrap();
            let settings = Config::new(initialization, workspace, None).unwrap();
            let output = forwarded(settings.resolved("vsrocqtop", &[]));
            assert!(output.proof.block);
            assert!(output.proof.auto_open);
            assert!(output.bridge.is_none());
            assert!(output.vsrocq.is_none());
        }
    }

    #[test]
    fn similarly_named_unknown_settings_are_not_selected() {
        let settings = configured(
            r#"{"proof":{"block":false},
                "vsrocq":{"proof":{"block":true},"server":{"path":"ignored"},"bridge":{"path":"ignored"}}}"#,
            "{}",
        );
        assert_eq!(settings.server.path, None);
        assert_eq!(settings.bridge.path, None);
        let output = forwarded(settings.resolved("vsrocqtop", &[]));
        assert!(!output.proof.block);
        assert!(output.proof.auto_open);
        assert!(output.vsrocq.is_some());
    }

    #[test]
    fn rejects_non_boolean_and_explicit_null_proof_settings() {
        for text in [
            r#"{"proof":{"block":"false"}}"#,
            r#"{"proof":{"block":null}}"#,
            r#"{"proof":{"autoOpen":null}}"#,
            r#"{"proof":null}"#,
        ] {
            let result = crate::decode_settings(Some(serde_json::from_str(text).unwrap()), "settings")
                .and_then(|input| Config::new(input, Settings::default(), None));
            let error = result.err().expect("invalid proof settings must fail");
            assert!(matches!(error, error::Error::Settings { .. }));
            assert!(std::error::Error::source(&error).is_some_and(|source| source.is::<vsrocq_settings::Error>()));
        }
        assert!(Config::new(input(r#"{"proof":null}"#), input(r#"{"proof":{"block":false}}"#), None,).is_err());
    }

    #[test]
    fn rejects_non_object_configuration_with_input_context() {
        let result = crate::decode_settings(Some(serde_json::from_str("[]").unwrap()), "initialization_options");
        let error::Error::DecodeConfiguration { section, source } =
            result.expect_err("configuration must be an object")
        else {
            panic!("expected a configuration decode error");
        };
        assert_eq!(section, "initialization_options");
        assert!(source.is_data());
    }

    #[test]
    fn preserves_merged_unknown_settings_and_resolved_opam_args() {
        let settings = configured(
            r#"{
                "foreign":{"value":["initialization"]},
                "server":{"path":"old","args":["old"],"local":true},
                "bridge":{"path":"bridge"},
                "proof":{"future":{"a":1,"nested":{"first":true}}},
                "future":{"a":3}
            }"#,
            r#"{
                "foreign":{"value":["workspace"]},
                "proof":{"future":{"b":2,"nested":{"second":false}}},
                "future":{"b":4}
            }"#,
        );
        let args = vec![
            "exec".into(),
            "--switch".into(),
            "/project".into(),
            "--".into(),
            "/opam/bin/vsrocqtop".into(),
        ];
        let output = forwarded(settings.resolved("/opam/bin/opam", &args));
        assert_eq!(output.foreign.unwrap().value, ["workspace"]);
        assert_eq!(output.server.path, "/opam/bin/opam");
        assert_eq!(output.server.args, args);
        assert!(output.bridge.is_none());
        assert!(output.vsrocq.is_none());
        let future = output.proof.future.unwrap();
        assert_eq!(future.a, Some(1));
        assert_eq!(future.b, Some(2));
        let nested = future.nested.unwrap();
        assert!(nested.first);
        assert!(!nested.second);
        let future = output.future.unwrap();
        assert_eq!(future.a, Some(3));
        assert_eq!(future.b, Some(4));
    }

    #[test]
    fn retains_all_bridge_overrides_and_nullable_path_precedence() {
        let settings = configured(
            r#"{"server":{"path":"initial","args":["--initial"]},
                "bridge":{"path":"initial","release_repository":"George-Miao/zed-vsrocq",
                    "release_tag":"v1","download_url":"https://example.org/bridge"}}"#,
            r#"{"server":{"path":null},"bridge":{"path":null,"release_tag":"v2"}}"#,
        );
        assert_eq!(settings.server.path, None);
        assert_eq!(settings.server.args, ["--initial"]);
        assert_eq!(settings.bridge.path, None);
        assert_eq!(
            settings.bridge.release_repository.as_deref(),
            Some("George-Miao/zed-vsrocq")
        );
        assert_eq!(settings.bridge.release_tag.as_deref(), Some("v2"));
        assert_eq!(
            settings.bridge.download_url.as_deref(),
            Some("https://example.org/bridge")
        );
    }
}
