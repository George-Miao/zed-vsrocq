mod changes;
mod proof;

use std::collections::BTreeMap;

use async_lsp::lsp_types::{
    Position, PositionEncodingKind, SaveOptions, TextDocumentSyncKind, TextDocumentSyncOptions,
    TextDocumentSyncSaveOptions,
};
pub(crate) use changes::{Change, ChangeParams};
use ez_jsonrpc_types::template as rpc;
pub(crate) use proof::{Diagnostics, ProofView};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_ignored_fields::{DeserializeIgnoredFields, PreserveIgnoredFields, SerializeIgnoredFields};
use serde_json::value::{RawValue, to_raw_value};
use snafu::{ResultExt, Snafu};
use vsrocq_settings::ServerSettings;

pub(crate) type Raw = Box<RawValue>;
pub(crate) type Id = rpc::Id;
pub(crate) type Request = rpc::Request<String, Id, Raw>;
pub(crate) type Error = rpc::Error<Raw>;
pub(crate) type Response = rpc::Response<Option<Raw>, Raw, String, Id>;
pub(crate) type Message = rpc::Message<String, Id, Raw, Option<Raw>, Raw, String>;
pub(crate) type Reply = std::result::Result<Option<Raw>, Error>;
pub(crate) type Result<T, E = Failure> = std::result::Result<T, E>;

#[derive(Debug, Snafu)]
pub(crate) enum Failure {
    #[snafu(display("cannot serialize protocol value: {source}"))]
    EncodeValue { source: serde_json::Error },

    #[snafu(display("cannot decode protocol value: {source}"))]
    DecodeValue { source: serde_json::Error },

    #[snafu(display("cannot serialize parameters for {method}: {source}"))]
    EncodeParams { method: String, source: serde_json::Error },

    #[snafu(display("cannot decode parameters for {method}: {source}"))]
    DecodeParams { method: String, source: serde_json::Error },

    #[snafu(display("cannot serialize result for request {id:?}: {source}"))]
    EncodeResult { id: Id, source: serde_json::Error },

    #[snafu(display("cannot decode result for request {id:?}: {source}"))]
    DecodeResult { id: Id, source: serde_json::Error },

    #[snafu(display("cannot decode protocol field {key:?}: {source}"))]
    DecodeField { key: String, source: serde_json::Error },

    #[snafu(display("request {id:?} failed: {source}"))]
    Rejected { id: Id, source: Error },
}

pub(crate) fn raw<T: Serialize + ?Sized>(value: &T) -> Result<Raw> {
    to_raw_value(value).context(EncodeValueSnafu)
}

pub(crate) fn decode<T: DeserializeOwned>(value: &RawValue) -> Result<T> {
    serde_json::from_str(value.get()).context(DecodeValueSnafu)
}

pub(crate) fn params<T: DeserializeOwned>(request: &Request) -> Result<T> {
    serde_json::from_str(request.params.as_deref().map_or("null", RawValue::get)).with_context(|_| DecodeParamsSnafu {
        method: request.method.clone(),
    })
}

pub(crate) fn result<T: DeserializeOwned>(response: &Response) -> Result<T> {
    match &response.result {
        Ok(value) => {
            serde_json::from_str(value.as_deref().map_or("null", RawValue::get)).with_context(|_| DecodeResultSnafu {
                id: response.id.clone(),
            })
        }
        Err(error) => Err(Failure::Rejected {
            id: response.id.clone(),
            source: error.clone(),
        }),
    }
}

pub(crate) fn request<T: Serialize + ?Sized>(method: impl Into<String>, id: Option<Id>, params: &T) -> Result<Request> {
    let method = method.into();
    let params = to_raw_value(params).with_context(|_| EncodeParamsSnafu { method: method.clone() })?;
    Ok(Request {
        method,
        params: Some(params),
        id,
    })
}

pub(crate) fn reply<T: Serialize + ?Sized>(id: Id, result: &T) -> Result<Response> {
    let result = to_raw_value(result).with_context(|_| EncodeResultSnafu { id: id.clone() })?;
    Ok(Response {
        id,
        result: Ok(Some(result)),
    })
}

pub(crate) fn failure(id: Id, code: i64, message: impl Into<String>) -> Response {
    Response {
        id,
        result: Err(Error {
            code,
            message: message.into(),
            data: None,
        }),
    }
}

// Only extension-bearing protocol objects use this transient map. Every known
// field is decoded into its static type; unknown members remain opaque JSON.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(transparent)]
pub(crate) struct Fields(BTreeMap<String, Raw>);

impl Fields {
    pub(crate) fn take<T: DeserializeOwned>(&mut self, key: &str) -> Result<Option<T>> {
        self.0
            .remove(key)
            .map(|value| serde_json::from_str(value.get()).with_context(|_| DecodeFieldSnafu { key: key.to_owned() }))
            .transpose()
    }

    pub(crate) fn rest(self) -> BTreeMap<String, Raw> {
        self.0
    }
}

impl<'de> DeserializeIgnoredFields<'de> for Fields {
    type Key = String;
    type Value = Raw;

    fn insert<E: serde::de::Error>(&mut self, key: String, value: Raw) -> std::result::Result<(), E> {
        self.0.insert(key, value);
        Ok(())
    }
}

impl SerializeIgnoredFields for Fields {
    type Key = String;
    type Value = Raw;

    fn len(&self) -> usize {
        self.0.len()
    }

    fn iter(&self) -> impl Iterator<Item = (&String, &Raw)> {
        self.0.iter()
    }
}

pub(crate) type Extensible<T> = PreserveIgnoredFields<T, Fields>;

#[derive(Debug, Deserialize)]
#[serde(try_from = "Fields")]
pub(crate) struct InitializeInput {
    pub(crate) settings: Option<Raw>,
    pub(crate) capabilities: ClientCapabilities,
    pub(crate) workspace_folders: Option<Raw>,
    pub(crate) extra: BTreeMap<String, Raw>,
}

impl TryFrom<Fields> for InitializeInput {
    type Error = Failure;

    fn try_from(mut fields: Fields) -> Result<Self> {
        Ok(Self {
            settings: fields.take::<Option<Raw>>("initializationOptions")?.flatten(),
            capabilities: fields.take("capabilities")?.unwrap_or_default(),
            workspace_folders: fields.take("workspaceFolders")?,
            extra: fields.rest(),
        })
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Initialize {
    pub(crate) initialization_options: ServerSettings,
    pub(crate) capabilities: ClientCapabilities,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) workspace_folders: Option<Raw>,

    #[serde(flatten)]
    pub(crate) extra: BTreeMap<String, Raw>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", try_from = "Fields")]
pub(crate) struct ClientCapabilities {
    pub(crate) general: GeneralCapabilities,
    pub(crate) workspace: WorkspaceCapabilities,

    #[serde(flatten)]
    extra: BTreeMap<String, Raw>,
}

impl TryFrom<Fields> for ClientCapabilities {
    type Error = Failure;

    fn try_from(mut fields: Fields) -> Result<Self> {
        Ok(Self {
            general: fields
                .take::<Option<GeneralCapabilities>>("general")?
                .flatten()
                .unwrap_or_default(),
            workspace: fields
                .take::<Option<WorkspaceCapabilities>>("workspace")?
                .flatten()
                .unwrap_or_default(),
            extra: fields.rest(),
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", try_from = "Fields")]
pub(crate) struct GeneralCapabilities {
    pub(crate) position_encodings: Vec<PositionEncodingKind>,

    #[serde(flatten)]
    extra: BTreeMap<String, Raw>,
}

impl TryFrom<Fields> for GeneralCapabilities {
    type Error = Failure;

    fn try_from(mut fields: Fields) -> Result<Self> {
        Ok(Self {
            position_encodings: fields.take("positionEncodings")?.unwrap_or_default(),
            extra: fields.rest(),
        })
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", try_from = "Fields")]
pub(crate) struct WorkspaceCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) configuration: Option<bool>,

    #[serde(flatten)]
    extra: BTreeMap<String, Raw>,
}

impl TryFrom<Fields> for WorkspaceCapabilities {
    type Error = Failure;

    fn try_from(mut fields: Fields) -> Result<Self> {
        Ok(Self {
            configuration: fields.take::<Option<bool>>("configuration")?.flatten(),
            extra: fields.rest(),
        })
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum Sync {
    Kind(TextDocumentSyncKind),
    Options(SyncOptions),
}

impl<'de> Deserialize<'de> for Sync {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = Raw::deserialize(deserializer)?;
        if let Ok(kind) = decode::<TextDocumentSyncKind>(&value) {
            return Ok(Self::Kind(kind));
        }
        decode(&value).map(Self::Options).map_err(serde::de::Error::custom)
    }
}

impl Default for Sync {
    fn default() -> Self {
        Self::Options(Extensible::from(TextDocumentSyncOptions {
            open_close: Some(true),
            change: Some(TextDocumentSyncKind::INCREMENTAL),
            save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                include_text: Some(false),
            })),
            ..TextDocumentSyncOptions::default()
        }))
    }
}

impl Sync {
    pub(crate) fn normalize(&mut self) {
        match self {
            Self::Kind(change) => {
                *self = Self::Options(Extensible::from(TextDocumentSyncOptions {
                    open_close: Some(true),
                    change: Some(*change),
                    save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                        include_text: Some(false),
                    })),
                    ..TextDocumentSyncOptions::default()
                }));
            }
            Self::Options(options) => {
                options.value.save = Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                    include_text: Some(false),
                }));
            }
        }
    }
}

type SyncOptions = Extensible<TextDocumentSyncOptions>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AtDocument<'a> {
    pub(crate) text_document: VersionedDocument<'a>,
    pub(crate) position: Position,
}

#[derive(Serialize)]
pub(crate) struct VersionedDocument<'a> {
    pub(crate) uri: &'a str,
    pub(crate) version: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DocumentParams<'a> {
    pub(crate) text_document: DocumentRef<'a>,
}

#[derive(Serialize)]
pub(crate) struct DocumentRef<'a> {
    pub(crate) uri: &'a str,
}

#[derive(Deserialize)]
pub(crate) struct DocumentState {
    pub(crate) document: String,
}

#[derive(Deserialize)]
pub(crate) struct DocumentProofs {
    #[serde(rename = "proofs")]
    pub(crate) _proofs: Vec<Raw>,
}

#[derive(Deserialize)]
pub(crate) struct BlockOnError {
    pub(crate) uri: String,
    pub(crate) range: async_lsp::lsp_types::Range,
}

#[derive(Serialize)]
pub(crate) struct Empty {}

#[cfg(test)]
mod tests {
    use super::{Fields, Initialize, InitializeInput, PositionEncodingKind, Raw, Sync, decode, raw};

    #[test]
    fn initialization_changes_only_owned_settings_and_capabilities() {
        let input: InitializeInput = serde_json::from_str(
            r#"{
            "processId":17,
            "rootUri":"file:///workspace",
            "initializationOptions":{"futureSetting":true},
            "capabilities":{
                "general":{"positionEncodings":["utf-8"],"futureGeneral":[1,null]},
                "workspace":{"configuration":false,"futureWorkspace":{"n":1.00e+2}},
                "experimental":{"opaque":["native",true]}
            },
            "workspaceFolders":[{"uri":"file:///workspace","name":"work","futureFolder":true}],
            "futureInitialize":{"opaque":1.00e+2}
        }"#,
        )
        .unwrap();
        assert_eq!(input.settings.as_deref().unwrap().get(), r#"{"futureSetting":true}"#);
        let mut capabilities = input.capabilities;
        capabilities.general.position_encodings = vec![PositionEncodingKind::UTF16];
        capabilities.workspace.configuration = Some(true);
        let initialize = Initialize {
            initialization_options: vsrocq_settings::ServerSettings::default(),
            capabilities,
            workspace_folders: input.workspace_folders,
            extra: input.extra,
        };
        let mut encoded: Fields = decode(&raw(&initialize).unwrap()).unwrap();
        let mut settings: Fields = encoded.take("initializationOptions").unwrap().unwrap();
        assert!(settings.take::<Raw>("vsrocq").unwrap().is_none());
        let mut proof: Fields = settings.take("proof").unwrap().unwrap();
        assert_eq!(proof.take::<bool>("block").unwrap(), Some(true));
        assert_eq!(encoded.take::<i64>("processId").unwrap(), Some(17));
        assert_eq!(
            encoded.take::<Raw>("futureInitialize").unwrap().unwrap().get(),
            r#"{"opaque":1.00e+2}"#
        );
        assert!(
            encoded
                .take::<Raw>("workspaceFolders")
                .unwrap()
                .unwrap()
                .get()
                .contains("futureFolder")
        );
        let mut capabilities: Fields = encoded.take("capabilities").unwrap().unwrap();
        assert_eq!(
            capabilities.take::<Raw>("experimental").unwrap().unwrap().get(),
            r#"{"opaque":["native",true]}"#
        );
        let mut general: Fields = capabilities.take("general").unwrap().unwrap();
        assert_eq!(
            general
                .take::<Vec<PositionEncodingKind>>("positionEncodings")
                .unwrap()
                .unwrap(),
            [PositionEncodingKind::UTF16]
        );
        assert_eq!(general.take::<Raw>("futureGeneral").unwrap().unwrap().get(), "[1,null]");
        let mut workspace: Fields = capabilities.take("workspace").unwrap().unwrap();
        assert_eq!(workspace.take::<bool>("configuration").unwrap(), Some(true));
        assert_eq!(
            workspace.take::<Raw>("futureWorkspace").unwrap().unwrap().get(),
            r#"{"n":1.00e+2}"#
        );
    }

    #[test]
    fn sync_normalization_preserves_native_extensions_and_save_notifications() {
        let mut sync: Sync = serde_json::from_str(
            r#"{
            "openClose":false,"change":1,"willSave":true,"willSaveWaitUntil":true,
            "save":true,"futureSync":{"opaque":[null,1.00e+2]}
        }"#,
        )
        .unwrap();
        sync.normalize();
        let mut encoded: Fields = decode(&raw(&sync).unwrap()).unwrap();
        assert_eq!(encoded.take::<bool>("openClose").unwrap(), Some(false));
        assert_eq!(encoded.take::<i32>("change").unwrap(), Some(1));
        assert_eq!(encoded.take::<bool>("willSave").unwrap(), Some(true));
        assert_eq!(encoded.take::<bool>("willSaveWaitUntil").unwrap(), Some(true));
        assert_eq!(
            encoded.take::<Raw>("save").unwrap().unwrap().get(),
            r#"{"includeText":false}"#
        );
        assert_eq!(
            encoded.take::<Raw>("futureSync").unwrap().unwrap().get(),
            r#"{"opaque":[null,1.00e+2]}"#
        );
    }
}
