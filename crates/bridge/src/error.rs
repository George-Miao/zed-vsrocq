use std::{io, sync::Arc};

use snafu::Snafu;

use crate::{app, cli, config, document, http, navigation, process, protocol, secure, wire};

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub(crate) enum Error {
    #[snafu(context(false), display("{source}"))]
    Cli { source: cli::Error },

    #[snafu(context(false), display("{source}"))]
    Config { source: config::Error },

    #[snafu(context(false), display("{source}"))]
    Document { source: document::Error },

    #[snafu(context(false), display("{source}"))]
    Navigation { source: navigation::Error },

    #[snafu(context(false), display("{source}"))]
    Process { source: process::Error },

    #[snafu(context(false), display("{source}"))]
    Http { source: http::Error },

    #[snafu(context(false), display("{source}"))]
    Secure { source: secure::Error },

    #[snafu(context(false), display("{source}"))]
    Wire { source: wire::Error },

    #[snafu(context(false), display("{source}"))]
    Protocol { source: protocol::Failure },

    #[snafu(context(false), display("{source}"))]
    Action { source: app::ActionError },

    #[snafu(context(false), display("{source}"))]
    Editor { source: app::EditorError },

    #[snafu(display("{source}"))]
    CommandFailed { source: Arc<Error> },

    #[snafu(display("cannot start bridge runtime: {source}"))]
    StartRuntime { source: io::Error },

    #[snafu(display("LSP connection failed: {source}"))]
    LspConnection { source: async_lsp::Error },

    #[snafu(display("cannot encode proof-view state: {source}"))]
    EncodeState { source: serde_json::Error },

    #[snafu(display("{method} is missing its request ID"))]
    MissingRequestId { method: &'static str },

    #[snafu(display("Zed session is shutting down"))]
    SessionShuttingDown,

    #[snafu(display("unknown VsRocq command: {command}"))]
    UnknownCommand { command: String },

    #[snafu(display("Zed session has not initialized yet"))]
    SessionNotInitialized,

    #[snafu(display("the proof-view document changed; wait for the current state before trying again"))]
    BrowserTargetChanged { uri: Option<String>, version: Option<i64> },

    #[snafu(display("didChange refers to an unopened document: {uri}"))]
    ChangeDocumentNotOpen { uri: String },

    #[snafu(display("proof document was closed: {uri}"))]
    ProofDocumentClosed { uri: String },

    #[snafu(display("proof session was stopped: {uri}"))]
    ProofSessionStopped { uri: String },

    #[snafu(display("invalid VsRocq block-on-error range: {source}"))]
    InvalidBlockRange { source: protocol::Failure },

    #[snafu(display("invalid VsRocq proof view: {source}"))]
    InvalidProofView { source: protocol::Failure },

    #[snafu(display("VsRocq documentProofs returned an unknown result shape: {source}"))]
    InvalidDocumentProofs { source: protocol::Failure },

    #[snafu(display("VsRocq documentState returned an unknown result shape: {source}"))]
    InvalidDocumentState { source: protocol::Failure },

    #[snafu(display("VsRocq {method} failed: {}", reply.message))]
    RocqRequestRejected {
        method: &'static str,
        reply: protocol::Error,
    },

    #[snafu(display("cannot open the browser URL: {source}"))]
    OpenUrl { source: url::ParseError },
}

impl Error {
    pub(crate) fn protocol_code(&self) -> i64 {
        match self {
            Self::Action { source } => source.code(),
            Self::CommandFailed { source } => source.protocol_code(),
            _ => async_lsp::lsp_types::error_codes::REQUEST_FAILED,
        }
    }
}
