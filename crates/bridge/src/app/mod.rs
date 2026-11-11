mod actions;
mod inlay;
mod lsp;

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, VecDeque},
    path::PathBuf,
    rc::Rc,
    sync::Arc,
};

pub(crate) use actions::Error as ActionError;
use async_lsp::{
    ClientSocket, MainLoop,
    lsp_types::{
        ConfigurationParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
        InlayHintParams, MessageActionItem, MessageType, PositionEncodingKind, ShowDocumentParams, ShowMessageParams,
        ShowMessageRequestParams,
    },
};
use compio::io::compat::{AsyncReadStream, AsyncWriteStream};
use flume::Sender;
pub(crate) use lsp::Error as EditorError;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use snafu::{OptionExt, ResultExt, ensure};

use crate::{
    Error, Result,
    cli::Options,
    config::{Config, SETUP_URL},
    document::{Document, Label, Position},
    error,
    http::Service,
    navigation::{Navigation, Target},
    process::{Reaper, Role, Server},
    protocol::{
        self, AtDocument, BlockOnError, Change, ChangeParams, Diagnostics, DocumentParams, DocumentProofs, DocumentRef,
        DocumentState, Empty, Fields, Highlights, Id, Initialize, InitializeInput, Message, ProofView, Raw, Request,
        Response, VersionedDocument,
    },
};

#[derive(Debug, Default, Serialize)]
struct BrowserState {
    revision: u64,
    document: Option<Label>,
    status: Status,
    proof: Option<Rc<ProofView>>,
    error: Option<BrowserError>,
    toolchain: Option<Toolchain>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Status {
    #[default]
    Idle,
    Checking,
    Ready,
    Error,
}

#[derive(Debug, Serialize)]
struct Toolchain {
    missing: bool,
    message: ToolchainMessage,
    url: Option<&'static str>,
}

#[derive(Debug)]
enum BrowserError {
    Failure(Arc<Error>),
    RocqMessage(String),
}

impl Serialize for BrowserError {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Self::Failure(error) => serializer.collect_str(&snafu::Report::from_error(error.as_ref())),
            Self::RocqMessage(message) => serializer.serialize_str(message),
        }
    }
}

#[derive(Debug)]
enum ToolchainMessage {
    Stopped(Arc<Error>),
    Unavailable(Arc<Error>),
}

impl Serialize for ToolchainMessage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let message = match self {
            Self::Stopped(error) => format!(
                "The running VsRocq server stopped: {error}. Run a proof command to start a new session.",
                error = snafu::Report::from_error(error.as_ref()),
            ),
            Self::Unavailable(error) => format!(
                "VsRocq could not start: {error}. Install Rocq and vsrocq-language-server, or set \
                 lsp.vsrocq.settings.server.path and lsp.vsrocq.settings.server.args in Zed. No tools are downloaded \
                 automatically.",
                error = snafu::Report::from_error(error.as_ref()),
            ),
        };
        serializer.serialize_str(&message)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommandInput {
    pub(crate) command: String,
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    position: Option<Position>,
    #[serde(default)]
    version: Option<i64>,
    #[serde(default)]
    source: Source,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Source {
    #[default]
    Editor,
    Browser,
}

pub(crate) enum Event {
    EditorResponse {
        id: Id,
        result: std::result::Result<serde_json::Value, async_lsp::ResponseError>,
    },
    Server {
        role: Role,
        epoch: u64,
        message: Message,
    },
    ServerClosed {
        role: Role,
        epoch: u64,
        error: Error,
    },
    Command {
        input: CommandInput,
        reply: flume::Sender<Result<()>>,
    },
    State(flume::Sender<Result<Vec<u8>>>),
}

struct ProofSession {
    server: Server,
    epoch: u64,
    uri: String,
    version: i64,
    ready: bool,
    queued: VecDeque<ProofAction>,
    last_view: Option<Rc<ProofView>>,
    navigation: Option<Navigation>,
    parse_query: bool,
    state_query: bool,
    expected: Option<Target>,
    reset_request: Option<String>,
    diagnostics: Vec<String>,
    highlights: Option<Highlights>,
    blocked: Option<Position>,
}

struct ProofAction {
    command: String,
    position: Option<Position>,
}

#[derive(Clone)]
enum Pending {
    Initialize,
    Recover,
    Shutdown,
    Forward,
    CodeActions(Vec<actions::LocalAction>),
    InlayHints(inlay::Query),
    Internal,
}

#[derive(Clone, Copy)]
enum ClientRequest {
    Open,
    Setup,
    InlayRefresh,
}

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(try_from = "Fields")]
struct Initialization {
    capabilities: actions::Capabilities,

    #[serde(flatten)]
    extra: BTreeMap<String, Raw>,
}

impl TryFrom<Fields> for Initialization {
    type Error = protocol::Failure;

    fn try_from(mut fields: Fields) -> protocol::Result<Self> {
        Ok(Self {
            capabilities: fields.take("capabilities")?.unwrap_or_default(),
            extra: fields.rest(),
        })
    }
}

#[derive(Serialize)]
struct ConfigurationChange<T> {
    settings: T,

    #[serde(flatten)]
    extra: BTreeMap<String, Raw>,
}

impl<'de> Deserialize<'de> for ConfigurationChange<Raw> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let mut fields = Fields::deserialize(deserializer)?;
        Ok(Self {
            settings: fields
                .take("settings")
                .map_err(serde::de::Error::custom)?
                .unwrap_or_else(|| RawValue::NULL.to_owned()),
            extra: fields.rest(),
        })
    }
}

struct App {
    root: PathBuf,
    config: Config,
    tx: Sender<Event>,
    socket: ClientSocket,
    replies: HashMap<Id, Sender<lsp::Reply>>,
    state: RefCell<BrowserState>,
    proof_url: String,
    browser_opened: bool,
    reaper: Reaper,
    documents: HashMap<String, Rc<Document>>,
    primary: Option<Server>,
    primary_epoch: u64,
    primary_ready: bool,
    native_actions: bool,
    native_hints: bool,
    native_hint_resolve: bool,
    hints: inlay::Hints,
    hint_refresh: inlay::Refresh,
    recovering: bool,
    primary_queue: VecDeque<Request>,
    pending: HashMap<Id, Pending>,
    initialize_params: Option<Initialize>,
    initialized: bool,
    proofs: HashMap<String, ProofSession>,
    proof_epoch: u64,
    client_requests: HashMap<String, ClientRequest>,
    request_serial: u64,
    request_prefix: String,
    setup_asked: bool,
    shutting_down: bool,
}

pub(crate) async fn run(options: Options) -> Result<()> {
    let (tx, rx) = flume::unbounded();
    let request_prefix = crate::secure::token()?;
    let mut service = Service::start(tx.clone()).await?;
    let reaper = Reaper::default();
    let (main_loop, socket) = MainLoop::new_server(|socket| App {
        config: Config::new(&options),
        root: options.root,
        tx,
        socket,
        replies: HashMap::new(),
        state: RefCell::new(BrowserState::default()),
        proof_url: service.url(),
        browser_opened: false,
        reaper: reaper.clone(),
        documents: HashMap::new(),
        primary: None,
        primary_epoch: 0,
        primary_ready: false,
        native_actions: false,
        native_hints: false,
        native_hint_resolve: false,
        hints: inlay::Hints::default(),
        hint_refresh: inlay::Refresh::default(),
        recovering: false,
        primary_queue: VecDeque::new(),
        pending: HashMap::new(),
        initialize_params: None,
        initialized: false,
        proofs: HashMap::new(),
        proof_epoch: 0,
        client_requests: HashMap::new(),
        request_serial: 0,
        request_prefix,
        setup_asked: false,
        shutting_down: false,
    });
    let events = compio::runtime::spawn(async move {
        while let Ok(event) = rx.recv_async().await {
            if socket.emit(event).is_err() {
                break;
            }
        }
    });
    let result = main_loop
        .run(
            AsyncReadStream::new(compio::fs::stdin()),
            AsyncWriteStream::new(compio::fs::stdout()),
        )
        .await;
    events.cancel().await;
    let stopped = service.stop().await.map_err(Error::from);
    let reaped = reaper.drain().await.map_err(Error::from);
    let result = match result {
        Ok(()) | Err(async_lsp::Error::Eof) => Ok(()),
        Err(source) => Err(source).context(error::LspConnectionSnafu),
    };
    result.and(stopped).and(reaped)
}

impl App {
    fn event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::EditorResponse { id, result } => self.client(Message::Response(lsp::native_response(id, result))),
            Event::Server { role, epoch, message } => self
                .server(role, epoch, message)
                .or_else(|error| self.server_closed(role, epoch, error)),
            Event::ServerClosed { role, epoch, error } => self.server_closed(role, epoch, error),
            Event::Command { input, reply } => {
                let result = self.command(input, false, false);
                let _ = reply.try_send(result);
                Ok(())
            }
            Event::State(reply) => {
                let state = serde_json::to_vec(&*self.state.borrow()).context(error::EncodeStateSnafu);
                let _ = reply.try_send(state);
                Ok(())
            }
        }
    }

    fn response<T: Serialize + ?Sized>(&mut self, id: Id, result: &T) -> Result<()> {
        self.output(Message::Response(protocol::reply(id, result)?))
    }

    fn response_error(&mut self, id: Id, code: i64, message: impl std::fmt::Display) -> Result<()> {
        self.output(Message::Response(protocol::failure(id, code, &message.to_string())))
    }

    fn response_failure(&mut self, id: Id, code: i64, error: &impl std::error::Error) -> Result<()> {
        self.response_error(id, code, snafu::Report::from_error(error))
    }

    fn state(&self, change: impl FnOnce(&mut BrowserState)) {
        let mut state = self.state.borrow_mut();
        change(&mut state);
        state.revision = state.revision.saturating_add(1);
    }

    fn next_id(&mut self) -> String {
        self.request_serial += 1;
        format!("vsrocq-bridge:{}:{}", &self.request_prefix[..16], self.request_serial)
    }

    fn client(&mut self, message: Message) -> Result<()> {
        let mut request = match message {
            Message::Response(response) => {
                if let Id::String(id) = &response.id {
                    if let Some(request) = self.client_requests.remove(id) {
                        if matches!(request, ClientRequest::InlayRefresh) {
                            self.hint_refresh.complete(response.result.is_ok());
                            self.refresh_hints();
                            return Ok(());
                        }
                        if matches!(request, ClientRequest::Setup)
                            && protocol::result::<Option<MessageActionItem>>(&response)
                                .ok()
                                .flatten()
                                .is_some_and(|action| action.title == "Open download and setup")
                        {
                            self.open_url(SETUP_URL.to_string())?;
                        }
                        return Ok(());
                    }
                }
                let error = self.primary.as_mut().and_then(|server| server.send(&response).err());
                if let Some(error) = error {
                    self.primary_failed(error)?;
                }
                return Ok(());
            }
            Message::Request(request) => request,
        };
        let method = request.method.as_str();
        if method == "initialize" {
            let id = request
                .id
                .clone()
                .context(error::MissingRequestIdSnafu { method: "initialize" })?;
            if self.initialize_params.is_some() {
                return self.response_error(id, -32600, "VsRocq was already initialized");
            }
            let input: InitializeInput = match protocol::params(&request) {
                Ok(input) => input,
                Err(error) => return self.response_failure(id, -32602, &error),
            };
            if let Err(error) = self.config.update(input.settings.as_deref().unwrap_or(RawValue::NULL)) {
                return self.response_failure(id, -32602, &error);
            }
            self.hint_refresh
                .support(input.capabilities.workspace.refresh_support());
            let mut capabilities = input.capabilities;
            capabilities.general.position_encodings = vec![PositionEncodingKind::UTF16];
            self.initialize_params = Some(Initialize {
                initialization_options: self.config.server_settings(),
                capabilities,
                workspace_folders: input.workspace_folders,
                extra: input.extra,
            });
            if let Err(error) = self.start_primary(id.clone(), false) {
                self.unavailable(Arc::new(error));
                let result = self.capabilities(Initialization::default());
                self.response(id, &result)?;
            }
            return Ok(());
        }
        if method == "initialized" {
            self.initialized = true;
            self.refresh_hints();
            self.ask_setup()?;
        }
        if method == "workspace/didChangeConfiguration" {
            let params: ConfigurationChange<Raw> = protocol::params(&request)?;
            let old = self.config.clone();
            if let Err(error) = self.config.update(&params.settings) {
                self.show_failure(&error)?;
                return Ok(());
            }
            self.clear_proofs();
            self.clear_current(Status::Idle);
            request.params = Some(protocol::raw(&ConfigurationChange {
                settings: self.config.server_settings(),
                extra: params.extra,
            })?);
            if !old.same_command(&self.config) || self.primary.is_none() {
                self.stop_primary("VsRocq server configuration changed")?;
                let id = Id::String(self.next_id());
                if let Err(error) = self.start_primary(id, true) {
                    self.unavailable(Arc::new(error));
                    self.ask_setup()?;
                }
                return Ok(());
            }
        }
        let mut save = None;
        if method == "textDocument/didOpen" {
            match protocol::params::<DidOpenTextDocumentParams>(&request) {
                Ok(params) => {
                    let document = Document::open(params.text_document);
                    self.invalidate_proof(&document.uri);
                    let label = document.label();
                    if self.view_is(&document.uri) {
                        self.state(|state| {
                            state.document = Some(label);
                            state.proof = None;
                            state.error = None;
                            state.status = Status::Idle;
                        });
                    }
                    self.hints.open(&document);
                    self.documents.insert(document.uri.clone(), Rc::new(document));
                }
                Err(error) => self.show_failure(&error)?,
            }
        } else if method == "textDocument/didChange" {
            let input: ChangeParams<Raw> = protocol::params(&request)?;
            let uri = input.text_document.value.uri.as_str().to_string();
            self.invalidate_proof(&uri);
            let changed = (|| -> Result<Vec<Change>> {
                let mut changes = protocol::decode::<Vec<Change>>(&input.content_changes)?;
                let document = self
                    .documents
                    .get_mut(&uri)
                    .with_context(|| error::ChangeDocumentNotOpenSnafu { uri: &uri })?;
                Rc::make_mut(document).change(
                    i64::from(input.text_document.value.version),
                    changes.iter_mut().map(|change| &mut change.value),
                )?;
                Ok(changes)
            })();
            let changes = match changed {
                Ok(changes) => changes,
                Err(error) => {
                    let error = Arc::new(error);
                    self.documents.remove(&uri);
                    self.hints.close(&uri);
                    self.state(|state| {
                        state.proof = None;
                        state.status = Status::Error;
                        state.error = Some(BrowserError::Failure(error.clone()));
                    });
                    self.show_failure(error.as_ref())?;
                    return Ok(());
                }
            };
            request.params = Some(protocol::raw(&ChangeParams {
                text_document: input.text_document,
                content_changes: changes,
                extra: input.extra,
            })?);
            if let Some(document) = self.documents.get(&uri) {
                self.hints.open(document);
            }
            if self.view_is(&uri) {
                let label = self.documents.get(&uri).map(|document| document.label());
                self.state(|state| {
                    state.document = label;
                    state.proof = None;
                    state.error = None;
                    state.status = Status::Idle;
                });
            }
        } else if method == "textDocument/didClose" {
            let params: DidCloseTextDocumentParams = protocol::params(&request)?;
            let uri = params.text_document.uri.as_str();
            self.invalidate_proof(uri);
            self.documents.remove(uri);
            self.hints.close(uri);
            if self.view_is(uri) {
                self.state(|state| {
                    state.document = None;
                    state.proof = None;
                    state.error = None;
                    state.status = Status::Idle;
                });
            }
        } else if method == "textDocument/didSave" {
            let params: DidSaveTextDocumentParams = protocol::params(&request)?;
            save = Some(params.text_document.uri.to_string());
        }
        if method == "shutdown" {
            self.shutting_down = true;
            self.clear_proofs();
            if self.primary.is_some() {
                self.forward(request, Pending::Shutdown)?;
            } else if let Some(id) = request.id {
                self.response(id, &())?;
            }
            return Ok(());
        }
        if method == "workspace/executeCommand" {
            let id = request.id.clone().context(error::MissingRequestIdSnafu {
                method: "executeCommand",
            })?;
            match actions::execution(request.params.as_deref().unwrap_or(RawValue::NULL)) {
                Ok(Some(input)) => {
                    return match self.command(input, true, false) {
                        Ok(()) => self.response(id, &()),
                        Err(error) => self.response_failure(id, error.protocol_code(), &error),
                    };
                }
                Ok(None) => {}
                Err(error) => return self.response_failure(id, error.code(), &error),
            }
        }
        if method == "textDocument/codeAction" {
            let id = request
                .id
                .clone()
                .context(error::MissingRequestIdSnafu { method: "codeAction" })?;
            let actions = match actions::collect(request.params.as_deref().unwrap_or(RawValue::NULL), &self.documents) {
                Ok(actions) => actions,
                Err(error) => return self.response_failure(id, error.code(), &error),
            };
            if self.primary.is_some() && self.native_actions {
                return self.forward(request, Pending::CodeActions(actions));
            }
            return self.response(id, &actions);
        }
        if method == "textDocument/inlayHint" {
            let id = request
                .id
                .clone()
                .context(error::MissingRequestIdSnafu { method: "inlayHint" })?;
            let params: InlayHintParams = match protocol::params(&request) {
                Ok(params) => params,
                Err(error) => return self.response_failure(id, -32602, &error),
            };
            if params.range.start > params.range.end {
                return self.response_error(id, -32602, "The inlay hint range is reversed");
            }
            let query = self.hints.query(params.text_document.uri.to_string(), params.range);
            if self.primary.is_some() && self.native_hints {
                return self.forward(request, Pending::InlayHints(query));
            }
            return self.inlay_response(&query, protocol::reply(id, &Vec::<Raw>::new())?);
        }
        if method == "inlayHint/resolve" {
            let id = request.id.clone().context(error::MissingRequestIdSnafu {
                method: "inlayHint/resolve",
            })?;
            let hint = request.params.as_deref().unwrap_or(RawValue::NULL);
            match protocol::inlay::resolve(hint) {
                Ok(Some(resolved)) => {
                    if !self.hints.resolves(&resolved) {
                        return self.response_error(id, -32801, "The proof progress hint is no longer current");
                    }
                    return self.response(id, hint);
                }
                Ok(None) if self.primary.is_some() && self.native_hint_resolve => {
                    return self.forward(request, Pending::Forward);
                }
                Ok(None) => return self.response_error(id, -32601, "Native inlay hint resolution is not supported"),
                Err(error) => return self.response_failure(id, -32602, &error),
            }
        }
        if !(self.recovering
            && matches!(
                method,
                "textDocument/didOpen" | "textDocument/didChange" | "textDocument/didClose"
            ))
        {
            self.forward(request, Pending::Forward)?;
        }
        if let Some(uri) = save {
            let version = self.documents.get(&uri).map(|document| document.version);
            let input = CommandInput {
                command: "run-to-end".into(),
                uri: Some(uri),
                position: None,
                version,
                source: Source::Editor,
            };
            if let Err(error) = self.command(input, false, true) {
                self.show_failure(&error)?;
            }
        }
        Ok(())
    }

    fn forward(&mut self, request: Request, kind: Pending) -> Result<()> {
        if self.primary.is_none() {
            if let Some(id) = request.id {
                self.response_error(
                    id,
                    -32002,
                    "VsRocq is unavailable; use VsRocq: Open proof view for setup guidance",
                )?;
            }
            return Ok(());
        }
        if let Some(id) = &request.id {
            self.pending.insert(id.clone(), kind);
        }
        if !self.primary_ready {
            self.primary_queue.push_back(request);
        } else if let Some(primary) = &mut self.primary {
            if let Err(error) = primary.send(&request) {
                self.primary_failed(error)?;
            }
        }
        Ok(())
    }

    fn start_primary(&mut self, id: Id, recover: bool) -> Result<()> {
        let mut params = self
            .initialize_params
            .clone()
            .context(error::SessionNotInitializedSnafu)?;
        params.initialization_options = self.config.server_settings();
        self.primary_epoch += 1;
        let mut primary = Server::start(
            &self.config,
            &self.root,
            Role::Primary,
            self.primary_epoch,
            self.tx.clone(),
            self.reaper.clone(),
        )?;
        primary.send(&protocol::request("initialize", Some(id.clone()), &params)?)?;
        self.pending
            .insert(id, if recover { Pending::Recover } else { Pending::Initialize });
        self.primary = Some(primary);
        self.primary_ready = false;
        self.recovering = recover;
        self.native_hints = false;
        self.native_hint_resolve = false;
        Ok(())
    }

    fn server(&mut self, role: Role, epoch: u64, message: Message) -> Result<()> {
        if role == Role::Proof {
            return self.proof_message(epoch, message);
        }
        if self.primary.is_none() || epoch != self.primary_epoch {
            return Ok(());
        }
        let response = match message {
            Message::Request(request) => {
                if request.method == "prover/proofView" {
                    return Ok(());
                }
                if request.method == "workspace/configuration" {
                    if let Ok(params) = protocol::params::<ConfigurationParams>(&request) {
                        if params
                            .items
                            .iter()
                            .all(|item| item.section.as_deref() == Some("vsrocq"))
                        {
                            let values = vec![self.config.server_settings(); params.items.len()];
                            if let (Some(primary), Some(id)) = (&mut self.primary, request.id) {
                                primary.send(&protocol::reply(id, &values)?)?;
                            }
                            return Ok(());
                        }
                    }
                }
                return self.output(Message::Request(request));
            }
            Message::Response(response) => response,
        };
        let kind = self.pending.remove(&response.id);
        match kind {
            Some(kind @ (Pending::Initialize | Pending::Recover)) => {
                let recover = matches!(kind, Pending::Recover);
                if let Err(error) = &response.result {
                    self.primary_failed(Error::RocqRequestRejected {
                        method: "initialize",
                        reply: error.clone(),
                    })?;
                    if !recover {
                        let result = self.capabilities(Initialization::default());
                        self.response(response.id, &result)?;
                    }
                    self.ask_setup()?;
                    return Ok(());
                }
                let initialized: Initialization = protocol::result(&response)?;
                self.primary_ready = true;
                self.recovering = false;
                self.setup_asked = false;
                self.state(|state| {
                    state.toolchain = None;
                    state.error = None;
                });
                let result = self.capabilities(initialized);
                if recover {
                    if let Some(primary) = &mut self.primary {
                        primary.send(&protocol::request("initialized", None, &Empty {})?)?;
                        for document in self.documents.values() {
                            primary.send(&open_document(document)?)?;
                        }
                    }
                } else {
                    self.response(response.id, &result)?;
                }
                while let Some(request) = self.primary_queue.pop_front() {
                    if let Some(primary) = &mut self.primary {
                        primary.send(&request)?;
                    }
                }
                Ok(())
            }
            Some(Pending::Internal) => {
                if let Err(error) = &response.result {
                    self.show_error(&error.message)?;
                }
                Ok(())
            }
            Some(Pending::Shutdown) => {
                self.output(Message::Response(response))?;
                self.primary.take();
                self.primary_ready = false;
                Ok(())
            }
            Some(Pending::CodeActions(actions)) => {
                self.output(Message::Response(actions::merge_response(response, actions)?))
            }
            Some(Pending::InlayHints(query)) => self.inlay_response(&query, response),
            Some(Pending::Forward) | None => self.output(Message::Response(response)),
        }
    }

    fn capabilities(&mut self, mut result: Initialization) -> Initialization {
        let capabilities = &mut result.capabilities;
        self.native_actions = actions::advertise(capabilities);
        self.native_hint_resolve = capabilities
            .inlay_hint_provider
            .as_ref()
            .is_some_and(protocol::inlay::Provider::resolve_support);
        self.native_hints = protocol::inlay::advertise(&mut capabilities.inlay_hint_provider);
        capabilities.position_encoding = Some(PositionEncodingKind::UTF16);
        capabilities
            .text_document_sync
            .get_or_insert_with(Default::default)
            .normalize();
        result
    }

    fn inlay_response(&mut self, query: &inlay::Query, response: Response) -> Result<()> {
        let id = response.id.clone();
        match self.hints.response(query, response) {
            Ok(response) => self.output(Message::Response(response)),
            Err(error) => self.response_failure(id, -32603, &error),
        }
    }

    fn command(&mut self, input: CommandInput, explicit: bool, save: bool) -> Result<()> {
        ensure!(!self.shutting_down, error::SessionShuttingDownSnafu);
        ensure!(is_action(&input.command), error::UnknownCommandSnafu {
            command: &input.command
        });
        ensure!(self.initialize_params.is_some(), error::SessionNotInitializedSnafu);
        if input.source == Source::Browser {
            let matches = browser_target(
                self.state.borrow().document.as_ref(),
                input.uri.as_deref(),
                input.version,
            );
            ensure!(matches, error::BrowserTargetChangedSnafu {
                uri: input.uri.clone(),
                version: input.version
            });
        }
        let (document, position) = input.target(&self.documents)?;
        let same = self
            .proofs
            .get(&document.uri)
            .is_some_and(|proof| proof.version == document.version);
        if !same || save {
            self.invalidate_proof(&document.uri);
        }
        if input.command != "open" || !self.view_is(&document.uri) {
            let label = document.label();
            let cached = if input.command == "open" {
                self.proofs.get(&document.uri).and_then(|proof| proof.last_view.clone())
            } else {
                None
            };
            let diagnostics = self
                .proofs
                .get(&document.uri)
                .filter(|_| input.command == "open")
                .map(|proof| proof.diagnostics.as_slice())
                .unwrap_or(&[]);
            let error = (!diagnostics.is_empty()).then(|| BrowserError::RocqMessage(diagnostics.join("\n\n")));
            let status = match cached.as_deref() {
                Some(proof) if proof.failed() || !diagnostics.is_empty() => Status::Error,
                Some(_) => Status::Ready,
                None if !diagnostics.is_empty() => Status::Error,
                None if matches!(input.command.as_str(), "open" | "reset" | "interrupt") => Status::Idle,
                None => Status::Checking,
            };
            self.state(|state| {
                state.document = Some(label);
                state.proof = cached;
                state.error = error;
                state.status = status;
            });
        }
        if should_open_browser(
            explicit,
            input.source,
            &input.command,
            self.config.auto_open,
            self.browser_opened,
        ) {
            self.open_browser()?;
        }
        if input.command == "open" {
            self.ask_setup()?;
            return Ok(());
        }
        if input.command == "interrupt" {
            // The server may not implement RPC interruption. Termination stops
            // both the visible check and mirrored execution, including prover
            // workers.
            self.clear_proofs();
            if self.primary.is_some() {
                self.stop_primary("VsRocq execution interrupted")?;
                let id = Id::String(self.next_id());
                if let Err(error) = self.start_primary(id, true) {
                    let error = Arc::new(error);
                    self.unavailable(error.clone());
                    self.ask_setup()?;
                    return Err(Error::CommandFailed { source: error });
                }
            }
            return Ok(());
        }
        if self.primary.is_none() {
            let id = Id::String(self.next_id());
            if let Err(error) = self.start_primary(id, true) {
                let error = Arc::new(error);
                self.unavailable(error.clone());
                self.ask_setup()?;
                return Err(Error::CommandFailed { source: error });
            }
        }
        if input.command == "reset" {
            self.invalidate_proof(&document.uri);
            let id = self.next_id();
            self.forward(
                protocol::request("prover/resetRocq", Some(Id::String(id)), &DocumentParams {
                    text_document: DocumentRef { uri: &document.uri },
                })?,
                Pending::Internal,
            )?;
            return Ok(());
        }
        if !self.proofs.contains_key(&document.uri) {
            self.proof_epoch += 1;
            let mut server = match Server::start(
                &self.config,
                &self.root,
                Role::Proof,
                self.proof_epoch,
                self.tx.clone(),
                self.reaper.clone(),
            ) {
                Ok(server) => server,
                Err(error) => {
                    let error = Arc::new(Error::from(error));
                    self.proof_error(&document.uri, error.clone());
                    return Err(Error::CommandFailed { source: error });
                }
            };
            let mut params = self
                .initialize_params
                .clone()
                .context(error::SessionNotInitializedSnafu)?;
            params.initialization_options = self.config.proof_settings();
            params.capabilities.workspace.configuration = Some(true);
            if let Err(error) = server.send(&protocol::request(
                "initialize",
                Some(Id::String("vsrocq-proof-initialize".into())),
                &params,
            )?) {
                let error = Arc::new(Error::from(error));
                self.proof_error(&document.uri, error.clone());
                return Err(Error::CommandFailed { source: error });
            }
            self.proofs.insert(document.uri.clone(), ProofSession {
                server,
                epoch: self.proof_epoch,
                uri: document.uri.clone(),
                version: document.version,
                ready: false,
                queued: VecDeque::new(),
                last_view: None,
                navigation: None,
                parse_query: false,
                state_query: false,
                expected: None,
                reset_request: None,
                diagnostics: Vec::new(),
                highlights: None,
                blocked: None,
            });
        }
        if let Some(proof) = self.proofs.get_mut(&document.uri) {
            proof.last_view = None;
            proof.expected = None;
            proof.reset_request = None;
            proof.highlights = None;
            proof.blocked = None;
            proof.queued.push_back(ProofAction {
                command: input.command,
                position,
            });
            self.hints.start(&document.uri, document.version, proof.epoch);
        }
        self.hints_changed();
        if let Err(error) = self.dispatch_proof(&document.uri) {
            let error = Arc::new(error);
            self.proof_error(&document.uri, error.clone());
            return Err(Error::CommandFailed { source: error });
        }
        Ok(())
    }

    fn dispatch_proof(&mut self, uri: &str) -> Result<()> {
        let document = self
            .documents
            .get(uri)
            .cloned()
            .with_context(|| error::ProofDocumentClosedSnafu { uri })?;
        let target = {
            let proof = self
                .proofs
                .get_mut(uri)
                .with_context(|| error::ProofSessionStoppedSnafu { uri })?;
            if !proof.ready {
                return Ok(());
            }
            let Some(navigation) = &mut proof.navigation else {
                if !proof.parse_query && !proof.state_query {
                    proof.parse_query = true;
                    proof.server.send(&protocol::request(
                        "prover/documentProofs",
                        Some(Id::String("vsrocq-proof-parse".into())),
                        &DocumentParams {
                            text_document: DocumentRef { uri },
                        },
                    )?)?;
                }
                return Ok(());
            };
            let mut target = None;
            while let Some(action) = proof.queued.pop_front() {
                target = Some(navigation.target(&document, &action.command, action.position)?);
            }
            target
        };
        let Some(target) = target else {
            return Ok(());
        };
        let (primary_message, proof_message, reset_request) = match target {
            Target::Sentence(range) => {
                let message = protocol::request("prover/interpretToPoint", None, &AtDocument {
                    text_document: VersionedDocument {
                        uri,
                        version: document.version,
                    },
                    position: range.end,
                })?;
                (message.clone(), message, None)
            }
            Target::Top => {
                let primary_id = self.next_id();
                let proof_id = self.next_id();
                let params = DocumentParams {
                    text_document: DocumentRef { uri },
                };
                (
                    protocol::request("prover/resetRocq", Some(Id::String(primary_id)), &params)?,
                    protocol::request("prover/resetRocq", Some(Id::String(proof_id.clone())), &params)?,
                    Some(proof_id),
                )
            }
        };
        self.forward(primary_message, Pending::Internal)?;
        if let Some(proof) = self.proofs.get_mut(uri) {
            proof.expected = Some(target);
            proof.reset_request = reset_request;
            proof.highlights = None;
            proof.blocked = None;
            proof.server.send(&proof_message)?;
        }
        self.hints_changed();
        Ok(())
    }

    fn publish_proof(&self, uri: &str) {
        if !self.view_is(uri) {
            return;
        }
        let Some(proof) = self.proofs.get(uri) else {
            return;
        };
        let view = proof.last_view.clone();
        let top_ready = proof.expected == Some(Target::Top) && proof.reset_request.is_none();
        let (status, error) = proof_presentation(view.as_deref(), &proof.diagnostics, top_ready);
        self.state(|state| {
            state.proof = view;
            state.status = status;
            state.error = error.map(BrowserError::RocqMessage);
        });
    }

    fn proof_message(&mut self, epoch: u64, message: Message) -> Result<()> {
        let Some(proof) = self.proofs.values().find(|proof| proof.epoch == epoch) else {
            return Ok(());
        };
        let Some(document) = self.documents.get(&proof.uri) else {
            return Ok(());
        };
        if !accept_proof(epoch, proof.epoch, &proof.uri, proof.version, document) {
            return Ok(());
        }
        let document = document.clone();
        let request = match message {
            Message::Response(response) => return self.proof_response(&document, response),
            Message::Request(request) => request,
        };
        match request.method.as_str() {
            "workspace/configuration" => {
                let params: ConfigurationParams = protocol::params(&request)?;
                let values: Vec<_> = params
                    .items
                    .iter()
                    .map(|item| {
                        (item.section.is_none() || item.section.as_deref() == Some("vsrocq"))
                            .then(|| self.config.proof_settings())
                    })
                    .collect();
                if let (Some(proof), Some(id)) = (self.proofs.get_mut(&document.uri), request.id) {
                    proof.server.send(&protocol::reply(id, &values)?)?;
                }
                return Ok(());
            }
            "prover/updateHighlights" => {
                let highlights: Highlights = protocol::params(&request).context(error::InvalidHighlightsSnafu)?;
                if highlights.uri != document.uri {
                    return Ok(());
                }
                if let Some(proof) = self.proofs.get_mut(&document.uri) {
                    proof.highlights = Some(highlights);
                }
                return self.publish_hints(&document.uri);
            }
            "textDocument/publishDiagnostics" => {
                let params: Diagnostics = protocol::params(&request)?;
                if params.uri != document.uri || params.version.is_some_and(|version| version != document.version) {
                    return Ok(());
                }
                if let Some(proof) = self.proofs.get_mut(&document.uri) {
                    proof.diagnostics = params.errors();
                    if !proof.diagnostics.is_empty() {
                        if let Some(highlights) = &mut proof.highlights {
                            highlights.processing_range.clear();
                        }
                    }
                }
                self.publish_hints(&document.uri)?;
                self.publish_proof(&document.uri);
                return self.dispatch_proof(&document.uri);
            }
            "prover/blockOnError" => {
                let params: BlockOnError = protocol::params(&request).context(error::InvalidBlockRangeSnafu)?;
                if params.uri != document.uri {
                    return Ok(());
                }
                document
                    .validate_position(params.range.start)
                    .context(error::HighlightPositionSnafu)?;
                if let Some(proof) = self.proofs.get_mut(&document.uri) {
                    proof.blocked = Some(params.range.start);
                    if let Some(highlights) = &mut proof.highlights {
                        highlights.processing_range.clear();
                    }
                    if let Some(navigation) = &mut proof.navigation {
                        navigation.blocked(&document, params.range)?;
                    }
                }
                self.publish_hints(&document.uri)?;
                self.hints_changed();
                return Ok(());
            }
            "prover/proofView" => {
                let view: ProofView = protocol::params(&request).context(error::InvalidProofViewSnafu)?;
                let expected = self.proofs.get(&document.uri).and_then(|proof| proof.expected);
                if expected != Some(Target::Sentence(view.range)) {
                    return Ok(());
                }
                if let Some(proof) = self.proofs.get_mut(&document.uri) {
                    if view.failed() {
                        proof.blocked = Some(
                            proof
                                .blocked
                                .map_or(view.range.start, |blocked| blocked.min(view.range.start)),
                        );
                    }
                    if let Some(highlights) = &mut proof.highlights {
                        highlights.processing_range.clear();
                    }
                    proof.last_view = Some(Rc::new(view));
                }
                self.publish_proof(&document.uri);
                self.publish_hints(&document.uri)?;
                self.hints_changed();
                return Ok(());
            }
            "window/showMessage" => {
                let params: ShowMessageParams = protocol::params(&request)?;
                if params.typ == MessageType::ERROR {
                    if let Some(highlights) = self
                        .proofs
                        .get_mut(&document.uri)
                        .and_then(|proof| proof.highlights.as_mut())
                    {
                        highlights.processing_range.clear();
                    }
                    self.hints.stop_checking(&document.uri);
                    self.hints_changed();
                }
                if params.typ == MessageType::ERROR && self.view_is(&document.uri) {
                    self.state(|state| {
                        state.status = Status::Error;
                        state.error = Some(BrowserError::RocqMessage(params.message));
                    });
                }
                return self.output(Message::Request(request));
            }
            _ => {}
        }
        if let Some(id) = request.id {
            let response = match request.method.as_str() {
                "window/workDoneProgress/create" | "client/registerCapability" | "client/unregisterCapability" => {
                    protocol::reply(id, &())?
                }
                "workspace/workspaceFolders" => protocol::reply(
                    id,
                    &self
                        .initialize_params
                        .as_ref()
                        .and_then(|params| params.workspace_folders.as_ref()),
                )?,
                _ => protocol::failure(id, -32601, "request is not available in the isolated proof session"),
            };
            if let Some(proof) = self.proofs.get_mut(&document.uri) {
                proof.server.send(&response)?;
            }
        }
        Ok(())
    }

    fn proof_response(&mut self, document: &Document, response: Response) -> Result<()> {
        let Id::String(id) = &response.id else {
            return Ok(());
        };
        match id.as_str() {
            "vsrocq-proof-initialize" => {
                if let Err(error) = &response.result {
                    return Err(Error::RocqRequestRejected {
                        method: "initialize",
                        reply: error.clone(),
                    });
                }
                if let Some(proof) = self.proofs.get_mut(&document.uri) {
                    proof.ready = true;
                    proof.server.send(&protocol::request("initialized", None, &Empty {})?)?;
                    proof.server.send(&open_document(document)?)?;
                }
                return self.dispatch_proof(&document.uri);
            }
            "vsrocq-proof-parse" => {
                if let Some(proof) = self.proofs.get_mut(&document.uri) {
                    proof.parse_query = false;
                }
                if let Err(error) = &response.result {
                    if error.code == -32802 {
                        // A real diagnostic publication triggers the next
                        // readiness query; no timer guesses parsing completion.
                        return Ok(());
                    }
                    return Err(Error::RocqRequestRejected {
                        method: "parser-state query",
                        reply: error.clone(),
                    });
                }
                let _: DocumentProofs = protocol::result(&response).context(error::InvalidDocumentProofsSnafu)?;
                if let Some(proof) = self.proofs.get_mut(&document.uri) {
                    proof.state_query = true;
                    proof.server.send(&protocol::request(
                        "prover/documentState",
                        Some(Id::String("vsrocq-proof-state".into())),
                        &DocumentParams {
                            text_document: DocumentRef { uri: &document.uri },
                        },
                    )?)?;
                }
                return Ok(());
            }
            "vsrocq-proof-state" => {
                if let Err(error) = &response.result {
                    return Err(Error::RocqRequestRejected {
                        method: "sentence-state query",
                        reply: error.clone(),
                    });
                }
                let state: DocumentState = protocol::result(&response).context(error::InvalidDocumentStateSnafu)?;
                let navigation = Navigation::parse(document, &state.document)?;
                if let Some(proof) = self.proofs.get_mut(&document.uri) {
                    proof.state_query = false;
                    proof.navigation = Some(navigation);
                }
                return self.dispatch_proof(&document.uri);
            }
            _ => {}
        }
        let reset_reply = self
            .proofs
            .get(&document.uri)
            .and_then(|proof| proof.reset_request.as_deref())
            == Some(id.as_str());
        if reset_reply {
            if let Err(error) = &response.result {
                return Err(Error::RocqRequestRejected {
                    method: "reset",
                    reply: error.clone(),
                });
            }
            if let Some(proof) = self.proofs.get_mut(&document.uri) {
                proof.reset_request = None;
                proof.last_view = None;
                proof.highlights = None;
                proof.blocked = None;
            }
            self.publish_proof(&document.uri);
            self.publish_hints(&document.uri)?;
            self.hints_changed();
        }
        Ok(())
    }

    fn server_closed(&mut self, role: Role, epoch: u64, error: Error) -> Result<()> {
        match role {
            Role::Primary if self.primary.is_some() && epoch == self.primary_epoch => self.primary_failed(error),
            Role::Proof => {
                if let Some(uri) = self
                    .proofs
                    .values()
                    .find(|proof| proof.epoch == epoch)
                    .map(|proof| proof.uri.clone())
                {
                    self.proof_error(&uri, Arc::new(error));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn primary_failed(&mut self, error: impl Into<Error>) -> Result<()> {
        let error = Arc::new(error.into());
        let was_ready = self.primary_ready;
        let pending = std::mem::take(&mut self.pending);
        self.primary.take();
        self.primary_ready = false;
        self.recovering = false;
        self.primary_queue.clear();
        if self.shutting_down {
            for (id, kind) in pending {
                if matches!(kind, Pending::Shutdown) {
                    self.response(id, &())?;
                }
            }
            return Ok(());
        }
        self.clear_proofs();
        if was_ready {
            self.state(|state| {
                state.proof = None;
                state.status = Status::Error;
                state.error = Some(BrowserError::Failure(error.clone()));
                state.toolchain = Some(Toolchain {
                    missing: false,
                    message: ToolchainMessage::Stopped(error.clone()),
                    url: None,
                });
            });
        } else {
            self.unavailable(error.clone());
        }
        for (id, kind) in pending {
            match kind {
                Pending::Initialize => {
                    let result = self.capabilities(Initialization::default());
                    self.response(id, &result)?;
                }
                Pending::Recover | Pending::Internal => {}
                _ => self.response_failure(id, -32002, error.as_ref())?,
            }
        }
        self.ask_setup()
    }

    fn stop_primary(&mut self, reason: &str) -> Result<()> {
        self.primary.take();
        self.primary_ready = false;
        self.primary_queue.clear();
        let pending = std::mem::take(&mut self.pending);
        for (id, kind) in pending {
            if !matches!(kind, Pending::Internal | Pending::Recover) {
                self.response_error(id, -32800, reason)?;
            }
        }
        Ok(())
    }

    fn invalidate_proof(&mut self, uri: &str) {
        // Dropping the owned process kills its whole worker group. The unique
        // process epoch rejects output already queued before termination,
        // including untagged proof views.
        self.proofs.remove(uri);
        self.hints.clear(uri);
        self.hints_changed();
    }

    fn clear_proofs(&mut self) {
        self.proofs.clear();
        self.hints.clear_all();
        self.hints_changed();
    }

    fn publish_hints(&mut self, uri: &str) -> Result<()> {
        let Some(proof) = self.proofs.get(uri) else {
            return Ok(());
        };
        let Some(document) = self.documents.get(uri) else {
            return Ok(());
        };
        if !accept_proof(proof.epoch, proof.epoch, uri, proof.version, document) {
            return Ok(());
        }
        let progress = match &proof.highlights {
            Some(highlights) => inlay::Progress::snapshot(
                document,
                proof.navigation.as_ref(),
                highlights,
                proof.expected,
                proof.blocked,
            )?,
            None => inlay::Progress::default(),
        };
        if self.hints.update(uri, proof.version, proof.epoch, progress) {
            self.hints_changed();
        }
        Ok(())
    }

    fn hints_changed(&mut self) {
        self.hint_refresh.changed();
        self.refresh_hints();
    }

    fn refresh_hints(&mut self) {
        if !self.initialized || self.shutting_down || !self.hint_refresh.request() {
            return;
        }
        let id = self.next_id();
        self.client_requests.insert(id.clone(), ClientRequest::InlayRefresh);
        let request = protocol::request("workspace/inlayHint/refresh", Some(Id::String(id.clone())), &());
        let sent = request
            .map_err(Error::from)
            .and_then(|request| self.output(Message::Request(request)));
        if sent.is_err() {
            self.client_requests.remove(&id);
            self.hint_refresh.complete(false);
        }
    }

    fn view_is(&self, uri: &str) -> bool {
        self.state
            .borrow()
            .document
            .as_ref()
            .is_some_and(|document| document.uri == uri)
    }

    fn clear_current(&self, status: Status) {
        self.state(|state| {
            state.proof = None;
            state.error = None;
            state.status = status;
        });
    }

    fn proof_error(&mut self, uri: &str, error: Arc<Error>) {
        self.invalidate_proof(uri);
        if self.view_is(uri) {
            self.state(|state| {
                state.proof = None;
                state.status = Status::Error;
                state.error = Some(BrowserError::Failure(error));
            });
        }
    }

    fn unavailable(&self, error: Arc<Error>) {
        self.state(|state| {
            state.proof = None;
            state.status = Status::Error;
            state.error = Some(BrowserError::Failure(error.clone()));
            state.toolchain = Some(Toolchain {
                missing: true,
                message: ToolchainMessage::Unavailable(error),
                url: Some(SETUP_URL),
            });
        });
    }

    fn show_failure(&mut self, error: &impl std::error::Error) -> Result<()> {
        self.show_error(snafu::Report::from_error(error))
    }

    fn show_error(&mut self, error: impl std::fmt::Display) -> Result<()> {
        self.output(Message::Request(protocol::request(
            "window/showMessage",
            None,
            &ShowMessageParams {
                typ: MessageType::ERROR,
                message: error.to_string(),
            },
        )?))
    }

    fn ask_setup(&mut self) -> Result<()> {
        let missing = self
            .state
            .borrow()
            .toolchain
            .as_ref()
            .is_some_and(|toolchain| toolchain.missing);
        if !self.initialized || !missing || self.setup_asked {
            return Ok(());
        }
        self.setup_asked = true;
        let id = self.next_id();
        self.client_requests.insert(id.clone(), ClientRequest::Setup);
        self.output(Message::Request(protocol::request(
            "window/showMessageRequest",
            Some(Id::String(id)),
            &ShowMessageRequestParams {
                typ: MessageType::WARNING,
                message: "VsRocq needs a local Rocq and vsrocq-language-server toolchain. Open the official download \
                          and setup instructions? Nothing will be downloaded or installed automatically."
                    .into(),
                actions: Some(vec![
                    MessageActionItem {
                        title: "Open download and setup".into(),
                        properties: HashMap::new(),
                    },
                    MessageActionItem {
                        title: "Not now".into(),
                        properties: HashMap::new(),
                    },
                ]),
            },
        )?))
    }

    fn open_browser(&mut self) -> Result<()> {
        self.open_url(self.proof_url.clone())?;
        self.browser_opened = true;
        Ok(())
    }

    fn open_url(&mut self, url: String) -> Result<()> {
        let id = self.next_id();
        self.client_requests.insert(id.clone(), ClientRequest::Open);
        self.output(Message::Request(protocol::request(
            "window/showDocument",
            Some(Id::String(id)),
            &ShowDocumentParams {
                uri: url.parse::<url::Url>().context(error::OpenUrlSnafu)?,
                external: Some(true),
                take_focus: Some(true),
                selection: None,
            },
        )?))
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.proofs.clear();
        self.primary.take();
    }
}

pub(crate) fn is_action(command: &str) -> bool {
    actions::is_action(command)
}

fn open_document(document: &Document) -> Result<Request> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Open<'a> {
        text_document: Item<'a>,
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Item<'a> {
        uri: &'a str,
        language_id: &'a str,
        version: i64,
        text: &'a str,
    }

    Ok(protocol::request("textDocument/didOpen", None, &Open {
        text_document: Item {
            uri: &document.uri,
            language_id: &document.language,
            version: document.version,
            text: &document.text,
        },
    })?)
}

fn accept_proof(epoch: u64, session_epoch: u64, uri: &str, version: i64, document: &Document) -> bool {
    epoch == session_epoch && uri == document.uri && version == document.version
}

fn should_open_browser(explicit: bool, source: Source, command: &str, auto_open: bool, opened: bool) -> bool {
    explicit && source == Source::Editor && (command == "open" || (auto_open && !opened))
}

fn proof_presentation(view: Option<&ProofView>, diagnostics: &[String], top_ready: bool) -> (Status, Option<String>) {
    let error = (!diagnostics.is_empty()).then(|| diagnostics.join("\n\n"));
    let status = if error.is_some() || view.is_some_and(ProofView::failed) {
        Status::Error
    } else if view.is_some() || top_ready {
        Status::Ready
    } else {
        Status::Checking
    };
    (status, error)
}

fn browser_target(document: Option<&Label>, uri: Option<&str>, version: Option<i64>) -> bool {
    document.is_some_and(|document| uri == Some(document.uri.as_str()) && version == Some(document.version))
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::HashMap, ffi::OsString, path::PathBuf, rc::Rc};

    use async_lsp::{MainLoop, lsp_types::InlayHint};

    use super::{
        App, BrowserState, ClientRequest, Source, Status, accept_proof, browser_target, inlay, lsp, proof_presentation,
        should_open_browser,
    };
    use crate::{
        cli::Options,
        config::Config,
        document::{Document, Position},
        process::Reaper,
        protocol::{self, Diagnostics, Id, Message, ProofView, Raw, Request},
    };

    #[test]
    fn untagged_proof_views_require_both_process_and_document_identity() {
        let document = Document {
            uri: "file:///b.v".into(),
            language: "rocq".into(),
            version: 8,
            text: String::new(),
        };
        assert!(accept_proof(4, 4, "file:///b.v", 8, &document));
        assert!(!accept_proof(3, 4, "file:///b.v", 8, &document));
        assert!(!accept_proof(4, 4, "file:///a.v", 8, &document));
        assert!(!accept_proof(4, 4, "file:///b.v", 7, &document));
    }

    #[test]
    fn browser_commands_require_the_rendered_document_and_version() {
        let document = crate::document::Label {
            uri: "file:///a.v".into(),
            name: "a.v".into(),
            version: 3,
        };
        assert!(browser_target(Some(&document), Some("file:///a.v"), Some(3)));
        assert!(!browser_target(Some(&document), Some("file:///b.v"), Some(3)));
        assert!(!browser_target(Some(&document), Some("file:///a.v"), Some(2)));
        assert!(!browser_target(Some(&document), None, Some(3)));
        assert!(!browser_target(Some(&document), Some("file:///a.v"), None));
        assert!(!browser_target(None, Some("file:///a.v"), Some(3)));
    }

    #[test]
    fn earlier_diagnostics_survive_an_empty_final_proof_message_list() {
        let diagnostics = serde_json::from_str::<Diagnostics>(r#"{"uri":"file:///proof.v","diagnostics":[
            {"severity":1,"range":{"start":{"line":2,"character":0}},"message":"The term I has type True while False was expected."},
            {"severity":2,"message":"A warning is not an execution error."}
        ]}"#).unwrap().errors();
        let view = ProofView::default();
        let (status, error) = proof_presentation(Some(&view), &diagnostics, false);
        assert_eq!(status, Status::Error);
        assert!(error.unwrap().contains("Line 3: The term I"));
        assert_eq!(proof_presentation(Some(&view), &[], false), (Status::Ready, None));
    }

    #[test]
    fn known_top_requires_reset_completion_and_never_reuses_an_old_view() {
        assert_eq!(proof_presentation(None, &[], false), (Status::Checking, None));
        assert_eq!(proof_presentation(None, &[], true), (Status::Ready, None));
    }

    fn with_app(test: impl FnOnce(&mut App)) {
        let options = Options {
            root: PathBuf::from("."),
            server: OsString::from("unused-test-server"),
            args: Vec::new(),
        };
        let (tx, _rx) = flume::unbounded();
        let (_main_loop, _socket) = MainLoop::new_server(|socket| {
            let mut app = App {
                root: options.root.clone(),
                config: Config::new(&options),
                tx,
                socket,
                replies: HashMap::new(),
                state: RefCell::new(BrowserState::default()),
                proof_url: "http://127.0.0.1:1/".into(),
                browser_opened: false,
                reaper: Reaper::default(),
                documents: HashMap::new(),
                primary: None,
                primary_epoch: 0,
                primary_ready: false,
                native_actions: false,
                native_hints: false,
                native_hint_resolve: false,
                hints: inlay::Hints::default(),
                hint_refresh: inlay::Refresh::default(),
                recovering: false,
                primary_queue: Default::default(),
                pending: HashMap::new(),
                initialize_params: None,
                initialized: false,
                proofs: HashMap::new(),
                proof_epoch: 0,
                client_requests: HashMap::new(),
                request_serial: 0,
                request_prefix: "test-request-prefix".into(),
                setup_asked: false,
                shutting_down: false,
            };
            test(&mut app);
            app
        });
    }

    fn notify(app: &mut App, method: &str, params: &str) {
        app.client(Message::Request(Request {
            method: method.into(),
            params: Some(serde_json::from_str(params).unwrap()),
            id: None,
        }))
        .unwrap();
    }

    fn request(app: &mut App, method: &str, params: Raw) -> lsp::Reply {
        let id = Id::Number(42.into());
        let (reply, response) = flume::bounded(1);
        app.replies.insert(id.clone(), reply);
        app.client(Message::Request(Request {
            method: method.into(),
            params: Some(params),
            id: Some(id),
        }))
        .unwrap();
        response.try_recv().unwrap()
    }

    fn inlays(app: &mut App, uri: &str, start: Position, end: Position) -> Vec<Raw> {
        let params = async_lsp::lsp_types::InlayHintParams {
            text_document: async_lsp::lsp_types::TextDocumentIdentifier {
                uri: uri.parse().unwrap(),
            },
            range: async_lsp::lsp_types::Range::new(start, end),
            work_done_progress_params: Default::default(),
        };
        let response = request(app, "textDocument/inlayHint", protocol::raw(&params).unwrap())
            .unwrap()
            .unwrap();
        protocol::decode(&response).unwrap()
    }

    #[test]
    fn editor_inlay_requests_work_without_native_support_and_clear_after_document_lifecycle_changes() {
        with_app(|app| {
            let uri = "file:///proof.v";
            notify(
                app,
                "textDocument/didOpen",
                r#"{"textDocument":{"uri":"file:///proof.v","languageId":"rocq","version":1,"text":"Check nat.\nCheck bool."}}"#,
            );
            app.hints.start(uri, 1, 3);
            app.hints.update(uri, 1, 3, inlay::Progress {
                checked: Some(Position::new(0, 10)),
                checking: Some(Position::new(1, 11)),
            });
            let hints = inlays(app, uri, Position::new(0, 10), Position::new(0, 10));
            assert_eq!(hints.len(), 1);
            let hint: InlayHint = protocol::decode(&hints[0]).unwrap();
            assert_eq!(hint.position, Position::new(0, 10));
            assert!(hint.kind.is_none());
            assert!(app.pending.is_empty());
            assert!(inlays(app, "file:///other.v", Position::new(0, 0), Position::new(1, 11)).is_empty());
            notify(
                app,
                "textDocument/didChange",
                r#"{"textDocument":{"uri":"file:///proof.v","version":2},"contentChanges":[{"text":"Check bool."}]}"#,
            );
            assert!(inlays(app, uri, Position::new(0, 0), Position::new(0, 11)).is_empty());
            app.hints.start(uri, 2, 4);
            app.hints.update(uri, 2, 4, inlay::Progress {
                checked: Some(Position::new(0, 11)),
                checking: None,
            });
            assert_eq!(inlays(app, uri, Position::new(0, 0), Position::new(0, 11)).len(), 1);
            notify(
                app,
                "textDocument/didClose",
                r#"{"textDocument":{"uri":"file:///proof.v"}}"#,
            );
            assert!(inlays(app, uri, Position::new(0, 0), Position::new(0, 11)).is_empty());
            notify(
                app,
                "textDocument/didOpen",
                r#"{"textDocument":{"uri":"file:///proof.v","languageId":"rocq","version":2,"text":"Check bool."}}"#,
            );
            assert!(inlays(app, uri, Position::new(0, 0), Position::new(0, 11)).is_empty());
        });
    }

    #[test]
    fn synthetic_hint_resolution_stays_local_and_rejects_moved_or_reset_progress() {
        with_app(|app| {
            let document = Rc::new(Document {
                uri: "file:///proof.v".into(),
                language: "rocq".into(),
                version: 1,
                text: "Check nat.\nCheck bool.".into(),
            });
            app.hints.open(&document);
            app.documents.insert(document.uri.clone(), document.clone());
            app.hints.start(&document.uri, 1, 3);
            app.hints.update(&document.uri, 1, 3, inlay::Progress {
                checked: Some(Position::new(0, 10)),
                checking: None,
            });
            let hint = inlays(app, &document.uri, Position::new(0, 0), Position::new(1, 11))
                .pop()
                .unwrap();
            let resolved = request(app, "inlayHint/resolve", hint.clone()).unwrap().unwrap();
            assert_eq!(resolved.get(), hint.get());
            app.hints.update(&document.uri, 1, 3, inlay::Progress {
                checked: None,
                checking: Some(Position::new(1, 11)),
            });
            let lsp::ReplyError::Native { source } = request(app, "inlayHint/resolve", hint.clone()).unwrap_err()
            else {
                panic!("stale bridge hints must return an LSP error");
            };
            assert_eq!(source.code, -32801);
            app.invalidate_proof(&document.uri);
            let lsp::ReplyError::Native { source } = request(app, "inlayHint/resolve", hint).unwrap_err() else {
                panic!("reset bridge hints must return an LSP error");
            };
            assert_eq!(source.code, -32801);
            let native = serde_json::from_str(
                r#"{"position":{"line":0,"character":2},"label":"native","data":{"vendor":true}}"#,
            )
            .unwrap();
            let lsp::ReplyError::Native { source } = request(app, "inlayHint/resolve", native).unwrap_err() else {
                panic!("unsupported native resolution must not reach VsRocq");
            };
            assert_eq!(source.code, -32601);
            assert!(app.pending.is_empty());
        });
    }

    #[test]
    fn rejected_global_refresh_responses_are_private_and_do_not_stop_hint_requests() {
        with_app(|app| {
            let id = "vsrocq-private-refresh".to_string();
            app.client_requests.insert(id.clone(), ClientRequest::InlayRefresh);
            app.client(Message::Response(protocol::failure(
                Id::String(id),
                -32601,
                "refresh not supported",
            )))
            .unwrap();
            assert!(app.client_requests.is_empty());
            assert!(inlays(app, "file:///proof.v", Position::new(0, 0), Position::new(0, 0)).is_empty());
        });
    }

    #[test]
    fn editor_proof_actions_auto_open_one_tab_but_explicit_open_remains_available() {
        for command in [
            "step-forward",
            "step-backward",
            "run-to-cursor",
            "run-to-end",
            "interrupt",
            "reset",
        ] {
            assert!(should_open_browser(true, Source::Editor, command, true, false));
            assert!(!should_open_browser(true, Source::Editor, command, true, true));
            assert!(!should_open_browser(true, Source::Editor, command, false, false));
            assert!(!should_open_browser(false, Source::Editor, command, true, false));
            assert!(!should_open_browser(true, Source::Browser, command, true, false));
        }
        assert!(should_open_browser(true, Source::Editor, "open", false, true));
        assert!(!should_open_browser(true, Source::Browser, "open", true, false));
    }
}
