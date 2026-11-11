use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    rc::Rc,
};

use async_lsp::lsp_types::{CodeActionKind, CodeActionParams, PositionEncodingKind, error_codes};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{MapAccess, Visitor, value::MapAccessDeserializer},
    ser::SerializeSeq,
};
use serde_json::value::RawValue;
use snafu::{OptionExt, ResultExt, Snafu, ensure};

use super::{CommandInput, Source};
use crate::{
    document::{Document, Position},
    protocol::{self, Fields, Raw},
};

const KIND: CodeActionKind = CodeActionKind::new("refactor.vsrocq");
const ACTIONS: &[Action] = &[
    Action {
        id: "vsrocq.step-forward",
        command: "step-forward",
        title: "VsRocq: Step Forward",
    },
    Action {
        id: "vsrocq.step-backward",
        command: "step-backward",
        title: "VsRocq: Step Backward",
    },
    Action {
        id: "vsrocq.run-to-cursor",
        command: "run-to-cursor",
        title: "VsRocq: Run to Cursor",
    },
    Action {
        id: "vsrocq.run-to-end",
        command: "run-to-end",
        title: "VsRocq: Run to End",
    },
    Action {
        id: "vsrocq.interrupt",
        command: "interrupt",
        title: "VsRocq: Interrupt",
    },
    Action {
        id: "vsrocq.reset",
        command: "reset",
        title: "VsRocq: Reset",
    },
    Action {
        id: "vsrocq.open",
        command: "open",
        title: "VsRocq: Open Proof View",
    },
];

struct Action {
    id: &'static str,
    command: &'static str,
    title: &'static str,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Target {
    uri: String,
    version: i64,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    position: Option<Position>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct LocalAction {
    title: &'static str,
    kind: CodeActionKind,
    command: LocalCommand,
}

#[derive(Clone, Debug, Serialize)]
struct LocalCommand {
    title: &'static str,
    command: &'static str,
    arguments: [Target; 1],
}

#[derive(Deserialize)]
struct ExecuteParams {
    command: String,
    arguments: Option<Raw>,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", try_from = "Fields")]
pub(super) struct Capabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    code_action_provider: Option<Provider>,

    #[serde(skip_serializing_if = "Option::is_none")]
    execute_command_provider: Option<Commands>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) inlay_hint_provider: Option<protocol::inlay::Provider>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) position_encoding: Option<PositionEncodingKind>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) text_document_sync: Option<protocol::Sync>,

    #[serde(flatten)]
    extras: BTreeMap<String, Raw>,
}

impl TryFrom<Fields> for Capabilities {
    type Error = protocol::Failure;

    fn try_from(mut fields: Fields) -> protocol::Result<Self> {
        Ok(Self {
            code_action_provider: fields.take::<Option<Provider>>("codeActionProvider")?.flatten(),
            execute_command_provider: fields.take::<Option<Commands>>("executeCommandProvider")?.flatten(),
            inlay_hint_provider: fields
                .take::<Option<protocol::inlay::Provider>>("inlayHintProvider")?
                .flatten(),
            position_encoding: fields
                .take::<Option<PositionEncodingKind>>("positionEncoding")?
                .flatten(),
            text_document_sync: fields.take::<Option<protocol::Sync>>("textDocumentSync")?.flatten(),
            extras: fields.rest(),
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum Provider {
    Simple(bool),
    Options(CodeActions),
}

impl<'de> Deserialize<'de> for Provider {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ProviderVisitor;

        impl<'de> Visitor<'de> for ProviderVisitor {
            type Value = Provider;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a code action support flag or options object")
            }

            fn visit_bool<E>(self, value: bool) -> std::result::Result<Self::Value, E> {
                Ok(Provider::Simple(value))
            }

            fn visit_map<M>(self, map: M) -> std::result::Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                CodeActions::deserialize(MapAccessDeserializer::new(map)).map(Provider::Options)
            }
        }

        deserializer.deserialize_any(ProviderVisitor)
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", try_from = "Fields")]
struct CodeActions {
    #[serde(skip_serializing_if = "Option::is_none")]
    code_action_kinds: Option<Vec<CodeActionKind>>,

    #[serde(flatten)]
    extras: BTreeMap<String, Raw>,
}

impl TryFrom<Fields> for CodeActions {
    type Error = protocol::Failure;

    fn try_from(mut fields: Fields) -> protocol::Result<Self> {
        Ok(Self {
            code_action_kinds: fields.take::<Option<Vec<CodeActionKind>>>("codeActionKinds")?.flatten(),
            extras: fields.rest(),
        })
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(try_from = "Fields")]
struct Commands {
    commands: Vec<String>,

    #[serde(flatten)]
    extras: BTreeMap<String, Raw>,
}

impl TryFrom<Fields> for Commands {
    type Error = protocol::Failure;

    fn try_from(mut fields: Fields) -> protocol::Result<Self> {
        Ok(Self {
            commands: fields
                .take::<Option<Vec<String>>>("commands")?
                .flatten()
                .unwrap_or_default(),
            extras: fields.rest(),
        })
    }
}

pub(super) type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(super)))]
pub(crate) enum Error {
    #[snafu(display("proof command is missing its document URI"))]
    MissingDocumentUri,

    #[snafu(display("the proof document is not open; request a code action in an open Rocq file"))]
    DocumentNotOpen { uri: String },

    #[snafu(display("proof command is missing its document version"))]
    MissingDocumentVersion { uri: String },

    #[snafu(display("the document changed after this command was created; request a new code action"))]
    ContentModified { uri: String, expected: i64, actual: i64 },

    #[snafu(display("run-to-cursor needs a code action position"))]
    MissingCursorPosition,

    #[snafu(display("{source}"))]
    InvalidPosition { source: crate::document::Error },

    #[snafu(display("invalid code action request: {source}"))]
    InvalidCodeAction { source: protocol::Failure },

    #[snafu(display("the code action selection has a reversed range"))]
    ReversedSelection { start: Position, end: Position },

    #[snafu(display("invalid executeCommand request: {source}"))]
    InvalidExecution { source: protocol::Failure },

    #[snafu(display("unknown VsRocq command: {command}"))]
    UnknownCommand { command: String },

    #[snafu(display("VsRocq commands need the document argument from a code action"))]
    MissingArguments,

    #[snafu(display("VsRocq commands need the document argument from a code action: {source}"))]
    InvalidArguments { source: protocol::Failure },

    #[snafu(display("VsRocq commands need one document argument from a code action"))]
    ArgumentCount { count: usize },

    #[snafu(display("invalid VsRocq code action argument: {source}"))]
    InvalidTarget { source: protocol::Failure },
}

impl Error {
    pub(crate) fn code(&self) -> i64 {
        match self {
            Self::ContentModified { .. } => error_codes::CONTENT_MODIFIED,
            _ => -32602,
        }
    }
}

impl CommandInput {
    pub(super) fn target(&self, documents: &HashMap<String, Rc<Document>>) -> Result<(Rc<Document>, Option<Position>)> {
        let uri = self.uri.as_deref().context(MissingDocumentUriSnafu)?;
        let document = documents.get(uri).with_context(|| DocumentNotOpenSnafu { uri })?;
        let version = self.version.with_context(|| MissingDocumentVersionSnafu { uri })?;
        ensure!(version == document.version, ContentModifiedSnafu {
            uri,
            expected: version,
            actual: document.version
        });
        let position = if self.command == "run-to-cursor" {
            let position = self.position.context(MissingCursorPositionSnafu)?;
            Some(document.validate_position(position).context(InvalidPositionSnafu)?)
        } else {
            None
        };
        Ok((document.clone(), position))
    }
}

pub(super) fn is_action(command: &str) -> bool {
    ACTIONS.iter().any(|action| action.command == command)
}

pub(super) fn collect(params: &RawValue, documents: &HashMap<String, Rc<Document>>) -> Result<Vec<LocalAction>> {
    let params: CodeActionParams = protocol::decode(params).context(InvalidCodeActionSnafu)?;
    if params.context.only.as_ref().is_some_and(|only| {
        !only.is_empty()
            && !only.iter().any(|kind| {
                let kind = kind.as_str();
                kind.is_empty()
                    || KIND.as_str() == kind
                    || KIND
                        .as_str()
                        .strip_prefix(kind)
                        .is_some_and(|suffix| suffix.starts_with('.'))
            })
    }) {
        return Ok(Vec::new());
    }
    let Some(document) = documents.get(params.text_document.uri.as_str()) else {
        return Ok(Vec::new());
    };
    document
        .validate_position(params.range.start)
        .context(InvalidPositionSnafu)?;
    document
        .validate_position(params.range.end)
        .context(InvalidPositionSnafu)?;
    ensure!(params.range.start <= params.range.end, ReversedSelectionSnafu {
        start: params.range.start,
        end: params.range.end
    });
    Ok(ACTIONS
        .iter()
        .map(|action| LocalAction {
            title: action.title,
            kind: KIND.clone(),
            command: LocalCommand {
                title: action.title,
                command: action.id,
                arguments: [Target {
                    uri: document.uri.clone(),
                    version: document.version,
                    position: (action.command == "run-to-cursor").then_some(params.range.end),
                }],
            },
        })
        .collect())
}

pub(super) fn execution(params: &RawValue) -> Result<Option<CommandInput>> {
    let params: ExecuteParams = protocol::decode(params).context(InvalidExecutionSnafu)?;
    if !params.command.starts_with("vsrocq.") {
        return Ok(None);
    }
    let action = ACTIONS
        .iter()
        .find(|action| action.id == params.command)
        .with_context(|| UnknownCommandSnafu {
            command: &params.command,
        })?;
    let arguments = params.arguments.context(MissingArgumentsSnafu)?;
    let arguments: Vec<Raw> = protocol::decode(&arguments).context(InvalidArgumentsSnafu)?;
    ensure!(arguments.len() == 1, ArgumentCountSnafu { count: arguments.len() });
    let target: Target = protocol::decode(&arguments[0]).context(InvalidTargetSnafu)?;
    Ok(Some(CommandInput {
        command: action.command.into(),
        uri: Some(target.uri),
        position: target.position,
        version: Some(target.version),
        source: Source::Editor,
    }))
}

pub(super) fn advertise(capabilities: &mut Capabilities) -> bool {
    let native = match capabilities.code_action_provider.as_mut() {
        Some(Provider::Simple(true)) => true,
        Some(Provider::Options(options)) => {
            if let Some(kinds) = &mut options.code_action_kinds {
                if !kinds.contains(&KIND) {
                    kinds.push(KIND.clone());
                }
            }
            true
        }
        _ => {
            capabilities.code_action_provider = Some(Provider::Options(CodeActions {
                code_action_kinds: Some(vec![KIND.clone()]),
                ..CodeActions::default()
            }));
            false
        }
    };
    let commands = &mut capabilities
        .execute_command_provider
        .get_or_insert_with(Commands::default)
        .commands;
    for action in ACTIONS {
        if !commands.iter().any(|command| command == action.id) {
            commands.push(action.id.into());
        }
    }
    native
}

pub(super) fn merge_response(
    mut response: protocol::Response,
    actions: Vec<LocalAction>,
) -> crate::Result<protocol::Response> {
    if actions.is_empty() {
        return Ok(response);
    }
    let result = match &response.result {
        Err(_) => return Ok(response),
        Ok(None) => protocol::raw(&actions)?,
        Ok(Some(result)) => {
            let Ok(native) = protocol::decode::<Vec<Raw>>(result) else {
                return Ok(response);
            };
            protocol::raw(&Merged {
                native: &native,
                local: &actions,
            })?
        }
    };
    response.result = Ok(Some(result));
    Ok(response)
}

struct Merged<'a> {
    native: &'a [Raw],
    local: &'a [LocalAction],
}

impl Serialize for Merged<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut actions = serializer.serialize_seq(Some(self.native.len() + self.local.len()))?;
        for action in self.native {
            actions.serialize_element(action)?;
        }
        for action in self.local {
            actions.serialize_element(action)?;
        }
        actions.end()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, HashMap},
        rc::Rc,
    };

    use async_lsp::lsp_types::{CodeActionKind, CodeActionParams, Range};
    use serde::Serialize;
    use serde_json::value::RawValue;

    use super::{Capabilities, Error, advertise, collect, execution, merge_response};
    use crate::{
        document::{Document, Position},
        protocol::{self, Raw},
    };

    fn documents() -> HashMap<String, Rc<Document>> {
        let document = Document {
            uri: "file:///proof.v".into(),
            language: "rocq".into(),
            version: 7,
            text: "a\u{1f9a4}\u{03bb}b\r\nCheck nat.\n".into(),
        };
        HashMap::from([(document.uri.clone(), Rc::new(document))])
    }

    fn raw(json: &str) -> Raw {
        RawValue::from_string(json.into()).unwrap()
    }

    fn fields(value: &impl Serialize) -> BTreeMap<String, Raw> {
        protocol::decode(&protocol::raw(value).unwrap()).unwrap()
    }

    fn request() -> Raw {
        raw(r#"{
            "textDocument":{"uri":"file:///proof.v"},
            "range":{"start":{"line":0,"character":1},"end":{"line":0,"character":4}},
            "context":{"diagnostics":[]}
        }"#)
    }

    #[test]
    fn cursor_commands_keep_the_selection_end_in_utf16() {
        let documents = documents();
        let actions = collect(&request(), &documents).unwrap();
        let action = actions
            .iter()
            .find(|action| action.command.command == "vsrocq.run-to-cursor")
            .unwrap();
        let input = execution(&protocol::raw(&action.command).unwrap()).unwrap().unwrap();
        let (document, position) = input.target(&documents).unwrap();
        assert_eq!(position, Some(Position { line: 0, character: 4 }));
        assert_eq!(document.byte_offset(position.unwrap()).unwrap(), 7);
        assert!(actions.iter().all(|action| !fields(action).contains_key("edit")));
    }

    #[test]
    fn local_actions_preserve_the_seven_command_contract() {
        let actions = collect(&request(), &documents()).unwrap();
        let expected = [
            ("vsrocq.step-forward", "step-forward", "VsRocq: Step Forward"),
            ("vsrocq.step-backward", "step-backward", "VsRocq: Step Backward"),
            ("vsrocq.run-to-cursor", "run-to-cursor", "VsRocq: Run to Cursor"),
            ("vsrocq.run-to-end", "run-to-end", "VsRocq: Run to End"),
            ("vsrocq.interrupt", "interrupt", "VsRocq: Interrupt"),
            ("vsrocq.reset", "reset", "VsRocq: Reset"),
            ("vsrocq.open", "open", "VsRocq: Open Proof View"),
        ];
        assert_eq!(actions.len(), expected.len());
        for (action, (id, command, title)) in actions.iter().zip(expected) {
            let serialized = fields(action);
            let kind: CodeActionKind = protocol::decode(&serialized["kind"]).unwrap();
            assert_eq!(kind.as_str(), "refactor.vsrocq");
            assert_eq!(action.title, title);
            assert_eq!(action.command.title, title);
            assert_eq!(action.command.command, id);
            let input = execution(&serialized["command"]).unwrap().unwrap();
            assert_eq!(input.command, command);
            assert_eq!(input.uri.as_deref(), Some("file:///proof.v"));
            assert_eq!(input.version, Some(7));
            assert_eq!(input.position.is_some(), command == "run-to-cursor");
        }
    }

    #[test]
    fn empty_selections_use_the_caret_without_a_coordinate_conversion() {
        let documents = documents();
        let mut params: CodeActionParams = protocol::decode(&request()).unwrap();
        params.range = Range {
            start: Position { line: 1, character: 6 },
            end: Position { line: 1, character: 6 },
        };
        let actions = collect(&protocol::raw(&params).unwrap(), &documents).unwrap();
        let action = actions
            .iter()
            .find(|action| action.command.command == "vsrocq.run-to-cursor")
            .unwrap();
        let input = execution(&protocol::raw(&action.command).unwrap()).unwrap().unwrap();
        assert_eq!(
            input.target(&documents).unwrap().1,
            Some(Position { line: 1, character: 6 })
        );
    }

    #[test]
    fn actions_never_fall_back_to_another_open_document() {
        let mut documents = documents();
        let other = Document {
            uri: "file:///other.v".into(),
            language: "rocq".into(),
            version: 7,
            text: "Check bool.".into(),
        };
        documents.insert(other.uri.clone(), Rc::new(other));
        let actions = collect(&request(), &documents).unwrap();
        for action in &actions {
            let input = execution(&protocol::raw(&action.command).unwrap()).unwrap().unwrap();
            let (document, _) = input.target(&documents).unwrap();
            assert_eq!(document.uri, "file:///proof.v");
        }
        documents.remove("file:///proof.v");
        assert!(collect(&request(), &documents).unwrap().is_empty());
        for action in &actions {
            let input = execution(&protocol::raw(&action.command).unwrap()).unwrap().unwrap();
            assert!(matches!(input.target(&documents), Err(Error::DocumentNotOpen { .. })));
        }
    }

    #[test]
    fn every_saved_action_rejects_a_new_document_version() {
        let mut documents = documents();
        let actions = collect(&request(), &documents).unwrap();
        Rc::make_mut(documents.get_mut("file:///proof.v").unwrap()).version = 8;
        for action in &actions {
            let input = execution(&protocol::raw(&action.command).unwrap()).unwrap().unwrap();
            let error = input.target(&documents).unwrap_err();
            assert_eq!(error.code(), -32801);
            assert!(matches!(error, Error::ContentModified {
                expected: 7,
                actual: 8,
                ..
            }));
        }
    }

    #[test]
    fn cursor_actions_reject_positions_inside_a_surrogate_pair() {
        let documents = documents();
        let mut params: CodeActionParams = protocol::decode(&request()).unwrap();
        params.range.end.character = 2;
        assert!(matches!(
            collect(&protocol::raw(&params).unwrap(), &documents),
            Err(Error::InvalidPosition {
                source: crate::document::Error::SplitSurrogate { .. }
            })
        ));
        let input = execution(&raw(r#"{
            "command":"vsrocq.run-to-cursor",
            "arguments":[{"uri":"file:///proof.v","version":7,"position":{"line":0,"character":2}}]
        }"#))
        .unwrap()
        .unwrap();
        assert!(matches!(
            input.target(&documents),
            Err(Error::InvalidPosition {
                source: crate::document::Error::SplitSurrogate { .. }
            })
        ));
    }

    #[test]
    fn reversed_code_action_ranges_are_rejected() {
        let mut params: CodeActionParams = protocol::decode(&request()).unwrap();
        params.range.start = Position { line: 1, character: 0 };
        let error = collect(&protocol::raw(&params).unwrap(), &documents()).unwrap_err();
        assert_eq!(error.code(), -32602);
        assert!(matches!(error, Error::ReversedSelection { .. }));
    }

    #[test]
    fn kind_filters_match_whole_hierarchical_components() {
        let documents = documents();
        for (only, included) in [
            (vec![], true),
            (vec![""], true),
            (vec!["quickfix"], false),
            (vec!["ref"], false),
            (vec!["refactor.extract"], false),
            (vec!["refactor"], true),
            (vec!["refactor.vsrocq"], true),
            (vec!["quickfix", "refactor"], true),
        ] {
            let mut params: CodeActionParams = protocol::decode(&request()).unwrap();
            params.context.only = Some(
                only.iter()
                    .map(|kind| CodeActionKind::from((*kind).to_owned()))
                    .collect(),
            );
            assert_eq!(
                !collect(&protocol::raw(&params).unwrap(), &documents)
                    .unwrap()
                    .is_empty(),
                included,
                "{only:?}"
            );
        }
    }

    #[test]
    fn native_actions_and_commands_keep_their_capabilities() {
        let mut capabilities: Capabilities = protocol::decode(&raw(r#"{
            "hoverProvider":true,
            "vendor":{"nested":[null,{"x":1.20e+3}]},
            "codeActionProvider":{"codeActionKinds":["quickfix"],"resolveProvider":true,"workDoneProgress":true,"x-provider":{"opaque":[true,null]}},
            "executeCommandProvider":{"commands":["native.explain"],"workDoneProgress":true,"x-commands":{"opaque":null}}
        }"#)).unwrap();
        assert!(advertise(&mut capabilities));
        let encoded = fields(&capabilities);
        assert_eq!(encoded["hoverProvider"].get(), "true");
        assert_eq!(encoded["vendor"].get(), r#"{"nested":[null,{"x":1.20e+3}]}"#);
        let provider: BTreeMap<String, Raw> = protocol::decode(&encoded["codeActionProvider"]).unwrap();
        assert_eq!(provider["resolveProvider"].get(), "true");
        assert_eq!(provider["workDoneProgress"].get(), "true");
        assert_eq!(provider["x-provider"].get(), r#"{"opaque":[true,null]}"#);
        let kinds: Vec<CodeActionKind> = protocol::decode(&provider["codeActionKinds"]).unwrap();
        assert_eq!(kinds[0].as_str(), "quickfix");
        assert_eq!(kinds[1].as_str(), "refactor.vsrocq");
        let provider: BTreeMap<String, Raw> = protocol::decode(&encoded["executeCommandProvider"]).unwrap();
        assert_eq!(provider["workDoneProgress"].get(), "true");
        assert_eq!(provider["x-commands"].get(), r#"{"opaque":null}"#);
        let commands: Vec<String> = protocol::decode(&provider["commands"]).unwrap();
        assert_eq!(commands[0], "native.explain");
        for action in collect(&request(), &documents()).unwrap() {
            assert!(commands.iter().any(|command| command == action.command.command));
        }
        assert!(advertise(&mut capabilities));
        let encoded = fields(&capabilities);
        let provider: BTreeMap<String, Raw> = protocol::decode(&encoded["executeCommandProvider"]).unwrap();
        let repeated: Vec<String> = protocol::decode(&provider["commands"]).unwrap();
        assert_eq!(commands, repeated);
        let provider: BTreeMap<String, Raw> = protocol::decode(&encoded["codeActionProvider"]).unwrap();
        let repeated: Vec<CodeActionKind> = protocol::decode(&provider["codeActionKinds"]).unwrap();
        assert_eq!(kinds, repeated);
        let native = raw(
            r#"{"command":"native.explain","arguments":[{"uri":"file:///proof.v","opaque":true},null,["opaque"],7]}"#,
        );
        assert!(execution(&native).unwrap().is_none());
    }

    #[test]
    fn absent_native_action_support_still_advertises_local_proof_actions() {
        for json in [
            "{}",
            r#"{"codeActionProvider":false}"#,
            r#"{"codeActionProvider":null}"#,
        ] {
            let mut capabilities: Capabilities = protocol::decode(&raw(json)).unwrap();
            assert!(!advertise(&mut capabilities));
            let encoded = fields(&capabilities);
            let provider: BTreeMap<String, Raw> = protocol::decode(&encoded["codeActionProvider"]).unwrap();
            let kinds: Vec<CodeActionKind> = protocol::decode(&provider["codeActionKinds"]).unwrap();
            let actions = collect(&request(), &documents()).unwrap();
            assert!(!actions.is_empty());
            for action in actions {
                assert!(kinds.contains(&action.kind));
            }
        }
    }

    #[test]
    fn native_true_and_unrestricted_options_keep_their_action_scope() {
        let mut capabilities: Capabilities = protocol::decode(&raw(r#"{"codeActionProvider":true}"#)).unwrap();
        assert!(advertise(&mut capabilities));
        assert_eq!(fields(&capabilities)["codeActionProvider"].get(), "true");
        let mut capabilities: Capabilities = protocol::decode(&raw(r#"{
            "codeActionProvider":{"resolveProvider":true,"x-provider":null}
        }"#))
        .unwrap();
        assert!(advertise(&mut capabilities));
        let encoded = fields(&capabilities);
        let provider: BTreeMap<String, Raw> = protocol::decode(&encoded["codeActionProvider"]).unwrap();
        assert!(!provider.contains_key("codeActionKinds"));
        assert_eq!(provider["resolveProvider"].get(), "true");
        assert_eq!(provider["x-provider"].get(), "null");
    }

    #[test]
    fn local_commands_require_exactly_one_strict_target() {
        for json in [
            r#"{"command":"vsrocq.step-forward"}"#,
            r#"{"command":"vsrocq.step-forward","arguments":[]}"#,
            r#"{"command":"vsrocq.step-forward","arguments":[null]}"#,
            r#"{"command":"vsrocq.step-forward","arguments":[{"uri":"file:///proof.v"}]}"#,
            r#"{"command":"vsrocq.step-forward","arguments":[{"uri":"file:///proof.v","version":7,"extra":true}]}"#,
            r#"{"command":"vsrocq.step-forward","arguments":[{"uri":"file:///proof.v","version":7},{"uri":"file:///proof.v","version":7}]}"#,
            r#"{"command":"vsrocq.unknown","arguments":[{"uri":"file:///proof.v","version":7}]}"#,
        ] {
            assert_eq!(execution(&raw(json)).unwrap_err().code(), -32602, "{json}");
        }
    }

    #[test]
    fn code_action_results_append_without_replacing_native_commands_or_edits() {
        let response: protocol::Response = protocol::decode(&raw(r#"{
            "jsonrpc":"2.0","id":12,
            "result":[{"title":"Explain","command":"native.explain","arguments":["opaque"],"data":{"native":[null,1.20e+3]}},{"title":"Repair","kind":"quickfix","edit":{"changes":{"file:///proof.v":[]}},"x-native":true}]
        }"#)).unwrap();
        let native: Vec<Raw> = protocol::decode(response.result.as_ref().unwrap().as_ref().unwrap()).unwrap();
        let actions = collect(&request(), &documents()).unwrap();
        let merged = merge_response(response, actions.clone()).unwrap();
        assert_eq!(protocol::raw(&merged.id).unwrap().get(), "12");
        let result: Vec<Raw> = protocol::decode(&merged.result.unwrap().unwrap()).unwrap();
        assert_eq!(result.len(), native.len() + actions.len());
        for (result, native) in result.iter().zip(&native) {
            assert_eq!(result.get(), native.get());
        }
        for (result, action) in result[native.len()..].iter().zip(&actions) {
            assert_eq!(result.get(), protocol::raw(action).unwrap().get());
        }
        let response: protocol::Response =
            protocol::decode(&raw(r#"{"jsonrpc":"2.0","id":13,"result":null}"#)).unwrap();
        let merged = merge_response(response, actions.clone()).unwrap();
        assert_eq!(protocol::raw(&merged.id).unwrap().get(), "13");
        assert_eq!(
            merged.result.unwrap().unwrap().get(),
            protocol::raw(&actions).unwrap().get()
        );
    }

    #[test]
    fn unexpected_native_result_shapes_are_untouched() {
        for json in [
            r#"{"jsonrpc":"2.0","id":14,"result":{"opaque":[null,1.20e+3]}}"#,
            r#"{"jsonrpc":"2.0","id":14,"result":"opaque"}"#,
            r#"{"jsonrpc":"2.0","id":14,"result":true}"#,
            r#"{"jsonrpc":"2.0","id":14,"result":17}"#,
        ] {
            let response: protocol::Response = protocol::decode(&raw(json)).unwrap();
            let original = protocol::raw(&response).unwrap();
            let merged = merge_response(response, collect(&request(), &documents()).unwrap()).unwrap();
            assert_eq!(protocol::raw(&merged).unwrap().get(), original.get());
        }
    }

    #[test]
    fn native_failures_are_not_hidden_by_available_proof_actions() {
        let error: protocol::Response = protocol::decode(&raw(r#"{
            "jsonrpc":"2.0","id":"native-request",
            "error":{"code":-32803,"message":"Native analysis failed","data":{"reason":"parser","opaque":[null,1.20e+3]}}
        }"#)).unwrap();
        let original = protocol::raw(&error).unwrap();
        let merged = merge_response(error, collect(&request(), &documents()).unwrap()).unwrap();
        assert_eq!(protocol::raw(&merged).unwrap().get(), original.get());
        let error = merged.result.unwrap_err();
        assert_eq!(error.code, -32803);
        assert_eq!(error.message, "Native analysis failed");
        assert_eq!(
            error.data.unwrap().get(),
            r#"{"reason":"parser","opaque":[null,1.20e+3]}"#
        );
        let empty: protocol::Response = protocol::decode(&raw(r#"{"jsonrpc":"2.0","id":14,"result":null}"#)).unwrap();
        let original = protocol::raw(&empty).unwrap();
        assert_eq!(
            protocol::raw(&merge_response(empty, Vec::new()).unwrap())
                .unwrap()
                .get(),
            original.get()
        );
    }
}
