use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;
use serde_repr::{Deserialize_repr, Serialize_repr};
use snafu::ResultExt;

use crate::{Field, Result, error::ParseSettingsSnafu, raw};

// Settings records share presence-aware merging and opaque future fields.
// Deserialization must visit the JSON map directly: Serde's flattened-content
// buffer cannot decode RawValue. Validate only after precedence is resolved.
macro_rules! record {
    (
        $name:ident ($section:literal) {
            fields { $( $(#[$field_meta:meta])* $field:ident: $ty:ty => $key:literal; )* }
            sections { $( $(#[$section_meta:meta])* $nested:ident: $nested_ty:ty => $nested_key:literal; )* }
        }
    ) => {
        #[derive(Clone, Debug, Default, Serialize)]
        pub struct $name {
            $(
                $(#[$field_meta])*
                #[serde(rename = $key, skip_serializing_if = "Field::is_missing")]
                pub $field: Field<$ty>,
            )*
            $(
                $(#[$section_meta])*
                #[serde(rename = $nested_key, skip_serializing_if = "Field::is_missing")]
                pub $nested: Field<$nested_ty>,
            )*
            #[serde(flatten)]
            extra: BTreeMap<String, Box<RawValue>>,
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let fields = BTreeMap::<String, Box<RawValue>>::deserialize(deserializer)?;
                Ok(Self::from_fields(fields))
            }
        }

        impl $name {
            pub(crate) fn from_fields(mut fields: BTreeMap<String, Box<RawValue>>) -> Self {
                Self {
                    $($field: raw::field(&mut fields, $key, concat!("vsrocq.", $section, $key)),)*
                    $($nested: raw::field(&mut fields, $nested_key, concat!("vsrocq.", $section, $nested_key)),)*
                    extra: fields,
                }
            }

            pub fn merge(&mut self, source: Self) -> Result<()> {
                $(self.$field.replace(source.$field);)*
                $(self.$nested.merge(source.$nested, <$nested_ty>::merge)?;)*
                raw::merge(&mut self.extra, source.extra)
            }

            pub fn validate(&self) -> Result<()> {
                $(self.$field.validate()?;)*
                $(
                    self.$nested.validate()?;
                    if let Some(value) = self.$nested.get() {
                        value.validate()?;
                    }
                )*
                Ok(())
            }
        }
    };
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Delegation {
    None,
    Skip,
    Delegate,
}

#[derive(Clone, Copy, Debug, Deserialize_repr, Eq, PartialEq, Serialize_repr)]
#[repr(u8)]
pub enum ProofMode {
    Manual     = 0,
    Continuous = 1,
}

#[derive(Clone, Copy, Debug, Deserialize_repr, Eq, PartialEq, Serialize_repr)]
#[repr(u8)]
pub enum PointInterpretation {
    Cursor      = 0,
    NextCommand = 1,
}

#[derive(Clone, Copy, Debug, Deserialize_repr, Eq, PartialEq, Serialize_repr)]
#[repr(u8)]
pub enum CompletionAlgorithm {
    SplitTypeIntersection = 0,
    StructuredSplitUnification = 1,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffMode {
    On,
    Off,
    Removed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PrettyPrint {
    Pp,
    String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Display {
    Tabs,
    List,
}

record! {
    Command ("server.") {
        fields {
            path: Option<String> => "path";
            args: Vec<String> => "args";
        }
        sections {}
    }
}

record! {
    Bridge ("bridge.") {
        fields {
            path: Option<String> => "path";
            release_repository: Option<String> => "release_repository";
            release_tag: Option<String> => "release_tag";
            download_url: Option<String> => "download_url";
        }
        sections {}
    }
}

record! {
    Cursor ("proof.cursor.") {
        fields { sticky: bool => "sticky"; }
        sections {}
    }
}

record! {
    Proof ("proof.") {
        fields {
            delegation: Delegation => "delegation";
            workers: Option<i64> => "workers";
            mode: ProofMode => "mode";
            block: bool => "block";
            point_interpretation_mode: PointInterpretation => "pointInterpretationMode";
            auto_open: bool => "autoOpen";
            display_buttons: bool => "display-buttons";
        }
        sections { cursor: Cursor => "cursor"; }
    }
}

record! {
    Diff ("goals.diff.") {
        fields { mode: DiffMode => "mode"; }
        sections {}
    }
}

record! {
    Messages ("goals.messages.") {
        fields { full: bool => "full"; }
        sections {}
    }
}

record! {
    Goals ("goals.") {
        fields {
            ppmode: Option<PrettyPrint> => "ppmode";
            auto: bool => "auto";
            display: Display => "display";
            max_depth: f64 => "maxDepth";
        }
        sections {
            diff: Diff => "diff";
            messages: Messages => "messages";
        }
    }
}

record! {
    Completion ("completion.") {
        fields {
            enable: bool => "enable";
            algorithm: CompletionAlgorithm => "algorithm";
            unification_limit: i64 => "unificationLimit";
            atomic_factor: f64 => "atomicFactor";
            size_factor: f64 => "sizeFactor";
        }
        sections {}
    }
}

record! {
    Diagnostics ("diagnostics.") {
        fields {
            enable: bool => "enable";
            full: bool => "full";
        }
        sections {}
    }
}

record! {
    Memory ("memory.") {
        fields { limit: i64 => "limit"; }
        sections {}
    }
}

record! {
    Interrupt ("interrupt.") {
        fields { preempt: bool => "preempt"; }
        sections {}
    }
}

record! {
    Settings ("") {
        fields {}
        sections {
            #[serde(skip_serializing)]
            server: Command => "server";
            #[serde(skip_serializing)]
            bridge: Bridge => "bridge";
            proof: Proof => "proof";
            goals: Goals => "goals";
            completion: Completion => "completion";
            diagnostics: Diagnostics => "diagnostics";
            memory: Memory => "memory";
            interrupt: Interrupt => "interrupt";
        }
    }
}

impl Settings {
    /// Decode one flat layer, deferring known-field validation until after
    /// merging.
    pub fn parse(value: &RawValue) -> Result<Self> {
        serde_json::from_str(value.get()).context(ParseSettingsSnafu)
    }

    pub fn extension_defaults() -> Self {
        Self {
            proof: Field::new(Proof {
                block: Field::new(true),
                auto_open: Field::new(true),
                ..Proof::default()
            }),
            ..Self::default()
        }
    }

    /// Serialize flat settings with the resolved command, omitting local bridge
    /// options.
    pub fn resolved<'a>(&'a self, path: &'a str, args: &'a [String]) -> impl Serialize + 'a {
        Resolved {
            settings: self,
            server: ResolvedCommand { path, args },
        }
    }
}

#[derive(Serialize)]
struct Resolved<'a> {
    #[serde(flatten)]
    settings: &'a Settings,
    server: ResolvedCommand<'a>,
}

#[derive(Serialize)]
struct ResolvedCommand<'a> {
    path: &'a str,
    args: &'a [String],
}

/// Complete backend settings. Routing settings and bridge-only settings are not
/// serialized to the prover.
#[derive(Clone, Debug, Serialize)]
pub struct ServerSettings {
    proof: Proof,
    goals: Goals,
    completion: Completion,
    diagnostics: Diagnostics,
    memory: Memory,
    interrupt: Interrupt,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            proof: Proof {
                delegation: Field::new(Delegation::None),
                workers: Field::new(None),
                mode: Field::new(ProofMode::Manual),
                block: Field::new(true),
                point_interpretation_mode: Field::new(PointInterpretation::Cursor),
                ..Proof::default()
            },
            goals: Goals {
                diff: Field::new(Diff {
                    mode: Field::new(DiffMode::Off),
                    ..Diff::default()
                }),
                messages: Field::new(Messages {
                    full: Field::new(true),
                    ..Messages::default()
                }),
                ppmode: Field::new(Some(PrettyPrint::Pp)),
                ..Goals::default()
            },
            completion: Completion {
                enable: Field::new(true),
                algorithm: Field::new(CompletionAlgorithm::StructuredSplitUnification),
                unification_limit: Field::new(100),
                ..Completion::default()
            },
            diagnostics: Diagnostics {
                enable: Field::new(true),
                full: Field::new(true),
                ..Diagnostics::default()
            },
            memory: Memory {
                limit: Field::new(4),
                ..Memory::default()
            },
            interrupt: Interrupt {
                preempt: Field::new(false),
                ..Interrupt::default()
            },
        }
    }
}

impl ServerSettings {
    pub fn from_settings(settings: Settings) -> Result<Self> {
        settings.validate()?;
        let mut resolved = Self::default();
        if let Some(proof) = settings.proof.into_value()? {
            resolved.proof.merge(proof)?;
        }
        if let Some(goals) = settings.goals.into_value()? {
            resolved.goals.merge(goals)?;
        }
        if let Some(completion) = settings.completion.into_value()? {
            resolved.completion.merge(completion)?;
        }
        if let Some(diagnostics) = settings.diagnostics.into_value()? {
            resolved.diagnostics.merge(diagnostics)?;
        }
        if let Some(memory) = settings.memory.into_value()? {
            resolved.memory.merge(memory)?;
        }
        if let Some(interrupt) = settings.interrupt.into_value()? {
            resolved.interrupt.merge(interrupt)?;
        }
        // Continuous checking would start work on every edit. Navigation also
        // requires exact sentence-end positions, not the following command.
        resolved.proof.mode = Field::new(ProofMode::Manual);
        resolved.proof.point_interpretation_mode = Field::new(PointInterpretation::Cursor);
        resolved.proof.auto_open = Field::default();
        Ok(resolved)
    }

    pub fn for_proof(&self) -> Self {
        let mut settings = self.clone();
        // Private diagnostics carry protocol state, regardless of the editor's
        // display preferences.
        settings.diagnostics.enable = Field::new(true);
        settings.diagnostics.full = Field::new(true);
        settings
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use serde_json::value::RawValue;

    use super::*;
    use crate::Error;

    fn parse(json: &str) -> Settings {
        Settings::parse(&RawValue::from_string(json.to_owned()).unwrap()).unwrap()
    }

    #[test]
    fn parse_errors_keep_the_operation_and_json_source() {
        for json in ["[]", "null", "false", r#""settings""#] {
            let raw = RawValue::from_string(json.to_owned()).unwrap();
            let Error::ParseSettings { source } = Settings::parse(&raw).unwrap_err() else {
                panic!("expected a settings object error");
            };
            assert!(source.is_data());
        }
    }

    #[test]
    fn similarly_named_unknown_fields_remain_opaque() {
        let settings = parse(r#"{"proof":{"block":true},"vsrocq":{"proof":{"block":false}}}"#);
        settings.validate().unwrap();
        assert_eq!(settings.proof.get().unwrap().block.get(), Some(&true));
        assert_eq!(settings.extra["vsrocq"].get(), r#"{"proof":{"block":false}}"#);
    }

    #[test]
    fn deferred_errors_keep_the_json_source_across_clones() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<Settings>();
        send_sync::<Error>();

        let settings = parse(r#"{"proof":{"block":"invalid"}}"#);
        let copy = settings.clone();
        let Error::InvalidSetting { field, source } = settings.validate().unwrap_err() else {
            panic!("expected an invalid setting");
        };
        assert_eq!(field, "vsrocq.proof.block");
        assert!(source.is_data());

        let proof = copy.proof.into_value().unwrap().unwrap();
        let Error::InvalidSetting {
            source: copied_source, ..
        } = proof.block.into_value().unwrap_err()
        else {
            panic!("expected the deferred setting error");
        };
        assert!(Arc::ptr_eq(&source, &copied_source));
    }

    #[test]
    fn higher_priority_fields_replace_errors_without_losing_presence() {
        let mut settings = parse(r#"{"proof":{"block":"invalid","workers":7,"mode":1,"cursor":{"sticky":true}}}"#);
        settings
            .merge(parse(r#"{"proof":{"block":false,"workers":null}}"#))
            .unwrap();
        settings.validate().unwrap();

        let proof = settings.proof.get().unwrap();
        assert_eq!(proof.block.get(), Some(&false));
        assert_eq!(proof.workers.get(), Some(&None));
        assert_eq!(proof.mode.get(), Some(&ProofMode::Continuous));
        assert_eq!(proof.cursor.get().unwrap().sticky.get(), Some(&true));
        assert!(proof.auto_open.is_missing());
    }

    #[test]
    fn nested_section_errors_follow_override_priority() {
        let mut settings = parse(r#"{"proof":false}"#);
        settings.merge(parse(r#"{"proof":{"block":false}}"#)).unwrap();
        settings.validate().unwrap();
        assert_eq!(settings.proof.get().unwrap().block.get(), Some(&false));

        settings.merge(parse(r#"{"proof":false}"#)).unwrap();
        let Error::InvalidSetting { field, source } = settings.validate().unwrap_err() else {
            panic!("expected the higher-priority section error");
        };
        assert_eq!(field, "vsrocq.proof");
        assert!(source.is_data());
    }

    #[test]
    fn raw_object_merges_apply_only_to_unknown_fields() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct Future {
            keep: i64,
            added: i64,
            replace: bool,
            deep: Deep,
        }

        #[derive(Debug, Deserialize, PartialEq)]
        struct Deep {
            left: i64,
            right: i64,
        }

        let mut settings = parse(
            r#"{
                "server":{"args":["old"]},
                "future":{"keep":1,"replace":[1,2],"deep":{"left":3}},
                "opaque":[ {"untouched" : [1, 2]} ]
            }"#,
        );
        settings
            .merge(parse(
                r#"{
                    "server":{"args":["new"]},
                    "future":{"added":2,"replace":false,"deep":{"right":4}}
                }"#,
            ))
            .unwrap();
        settings.validate().unwrap();

        assert_eq!(settings.server.get().unwrap().args.get().unwrap(), &["new"]);
        let future: Future = serde_json::from_str(settings.extra["future"].get()).unwrap();
        assert_eq!(future, Future {
            keep: 1,
            added: 2,
            replace: false,
            deep: Deep { left: 3, right: 4 },
        },);
        assert_eq!(settings.extra["opaque"].get(), r#"[ {"untouched" : [1, 2]} ]"#);
    }
}
