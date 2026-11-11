use std::{net::Ipv4Addr, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use compio::{
    net::TcpListener,
    runtime::{JoinError, JoinHandle},
};
use cyper_core::{CompioTimer, HyperStream};
use flume::Sender;
use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};
use hyper::server::conn::http1;
use hyper_util::{server::graceful::GracefulShutdown, service::TowerToHyperService};
use send_wrapper::SendWrapper;
use serde::Serialize;
use snafu::{ResultExt, Snafu};

use crate::{
    app::{CommandInput, Event, is_action},
    process::TaskFailure,
    secure,
};

#[derive(Debug, Snafu)]
pub(crate) enum Error {
    #[snafu(display("cannot create proof service token: {source}"))]
    AuthenticationToken { source: secure::Error },

    #[snafu(display("cannot bind proof service: {source}"))]
    BindListener { source: std::io::Error },

    #[snafu(display("cannot read proof service address: {source}"))]
    ListenerAddress { source: std::io::Error },

    #[snafu(display("cannot accept proof service connection: {source}"))]
    AcceptConnection { source: std::io::Error },

    #[snafu(display("cannot read proof service peer address: {source}"))]
    PeerAddress { source: std::io::Error },

    #[snafu(display("proof service task failed: {source}"))]
    ServiceTask {
        #[snafu(source(from(JoinError, TaskFailure::from)))]
        source: TaskFailure,
    },

    #[snafu(display("duplicate {name} header"))]
    DuplicateHeader { name: &'static str },
}

pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;

const MAX_BODY: usize = 64 * 1024;
const MAX_HEADER: usize = 16 * 1024;
const HEADER_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(7);
const CONNECTION_TIMEOUT: Duration = Duration::from_secs(10);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);
const INDEX: &str = include_str!("../../../web/index.html");
const SCRIPT: &str = include_str!(concat!(env!("OUT_DIR"), "/app.js"));
const STYLE: &str = include_str!("../../../web/style.css");

#[derive(Serialize)]
struct Accepted {
    accepted: bool,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

#[derive(Clone)]
struct Context {
    tx: Sender<Event>,
    host: String,
    origin: String,
    authorization: String,
}

impl Context {
    fn new(port: u16, token: &str, tx: Sender<Event>) -> Self {
        let host = format!("127.0.0.1:{port}");
        Self {
            tx,
            origin: format!("http://{host}"),
            host,
            authorization: format!("Bearer {token}"),
        }
    }
}

pub(crate) struct Service {
    stop: flume::Sender<()>,
    task: Option<JoinHandle<Result<()>>>,
    port: u16,
    token: String,
}

impl Service {
    pub(crate) async fn start(tx: Sender<Event>) -> Result<Self> {
        let token = secure::token().context(AuthenticationTokenSnafu)?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .context(BindListenerSnafu)?;
        let port = listener.local_addr().context(ListenerAddressSnafu)?.port();
        let context = Context::new(port, &token, tx);
        let (stop, shutdown) = flume::bounded(1);
        let task = compio::runtime::spawn(serve(listener, router(context), shutdown));
        Ok(Self {
            stop,
            task: Some(task),
            port,
            token,
        })
    }

    pub(crate) async fn stop(&mut self) -> Result<()> {
        let _ = self.stop.try_send(());
        match self.task.take() {
            Some(task) => task.await.context(ServiceTaskSnafu)?,
            None => Ok(()),
        }
    }

    pub(crate) fn url(&self) -> String {
        format!("http://127.0.0.1:{}/#token={}", self.port, self.token)
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.stop.try_send(());
    }
}

async fn serve(listener: TcpListener, router: Router, shutdown: flume::Receiver<()>) -> Result<()> {
    let graceful = GracefulShutdown::new();
    let mut connections = FuturesUnordered::new();
    // Keep the library's pending accept operation alive when a connection ends.
    let mut incoming = listener.incoming();
    let mut builder = http1::Builder::new();
    builder
        .timer(CompioTimer)
        .header_read_timeout(HEADER_TIMEOUT)
        .max_buf_size(MAX_HEADER)
        .keep_alive(false);
    let result = loop {
        let accepted = futures_util::select_biased! {
            _ = shutdown.recv_async().fuse() => break Ok(()),
            _ = async {
                if connections.is_empty() {
                    std::future::pending::<()>().await;
                }
                connections.next().await
            }.fuse() => continue,
            accepted = incoming.next().fuse() => accepted,
        };
        let stream = match accepted {
            Some(Ok(stream)) => stream,
            Some(Err(error)) => break Err(error).context(AcceptConnectionSnafu),
            None => break Ok(()),
        };
        let peer = match stream.peer_addr() {
            Ok(peer) => peer,
            Err(error) => break Err(error).context(PeerAddressSnafu),
        };
        if !peer.ip().is_loopback() {
            continue;
        }
        let connection = builder.serve_connection(
            Box::pin(HyperStream::new_plain(stream)),
            TowerToHyperService::new(router.clone()),
        );
        let watcher = graceful.watcher();
        connections.push(compio::runtime::spawn(async move {
            let _ = compio::time::timeout(CONNECTION_TIMEOUT, watcher.watch(connection)).await;
        }));
    };
    drop(incoming);
    drop(listener);
    let _ = compio::time::timeout(SHUTDOWN_TIMEOUT, graceful.shutdown()).await;
    for task in connections {
        let _ = task.cancel().await;
    }
    result
}

fn router(context: Context) -> Router {
    Router::new()
        .route("/", get(|| async { asset("text/html; charset=utf-8", INDEX) }))
        .route(
            "/index.html",
            get(|| async { asset("text/html; charset=utf-8", INDEX) }),
        )
        .route(
            "/app.js",
            get(|| async { asset("text/javascript; charset=utf-8", SCRIPT) }),
        )
        .route("/style.css", get(|| async { asset("text/css; charset=utf-8", STYLE) }))
        .route("/api/state", get(state))
        .route("/api/command", post(command))
        .fallback(|| async { error(StatusCode::NOT_FOUND, "not found") })
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn_with_state(context.clone(), guard))
        .with_state(context)
}

async fn guard(State(context): State<Context>, request: Request, next: Next) -> Response {
    let mut response = match authorize(&context, request.headers(), request.uri().path()) {
        Ok(()) => {
            // Axum requires Send futures; the wrapper stays on the single
            // Compio runtime thread.
            match SendWrapper::new(compio::time::timeout(REQUEST_TIMEOUT, next.run(request))).await {
                Ok(response) => response,
                Err(_) => error(StatusCode::REQUEST_TIMEOUT, "HTTP request exceeded its deadline"),
            }
        }
        Err(response) => response,
    };
    let headers = response.headers_mut();
    for (name, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "no-referrer"),
        ("cross-origin-resource-policy", "same-origin"),
        (
            "content-security-policy",
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; \
             frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        ),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}

fn authorize(context: &Context, headers: &HeaderMap, path: &str) -> Result<(), Response> {
    if !matches!(single_header(headers, "host"), Ok(Some(value)) if value.as_bytes() == context.host.as_bytes()) {
        return Err(error(StatusCode::FORBIDDEN, "invalid Host header"));
    }
    match single_header(headers, "origin") {
        Ok(None) => {}
        Ok(Some(value)) if value.as_bytes() == context.origin.as_bytes() => {}
        _ => {
            return Err(error(StatusCode::FORBIDDEN, "cross-origin requests are not accepted"));
        }
    }
    if path.starts_with("/api/")
        && !matches!(single_header(headers, "authorization"), Ok(Some(value)) if constant_eq(value.as_bytes(), context.authorization.as_bytes()))
    {
        return Err(error(
            StatusCode::UNAUTHORIZED,
            "authentication is required; reopen the proof view from Zed",
        ));
    }
    Ok(())
}

fn single_header<'a>(headers: &'a HeaderMap, name: &'static str) -> Result<Option<&'a HeaderValue>> {
    let mut values = headers.get_all(name).iter();
    let first = values.next();
    if values.next().is_some() {
        return DuplicateHeaderSnafu { name }.fail();
    }
    Ok(first)
}

async fn state(State(context): State<Context>) -> Response {
    let (reply, result) = flume::bounded(1);
    if context.tx.send(Event::State(reply)).is_err() {
        return error(StatusCode::SERVICE_UNAVAILABLE, "Zed session has stopped");
    }
    match SendWrapper::new(compio::time::timeout(Duration::from_secs(3), result.recv_async())).await {
        Ok(Ok(Ok(bytes))) => ([(header::CONTENT_TYPE, "application/json")], bytes).into_response(),
        Ok(Ok(Err(error))) => failure(StatusCode::SERVICE_UNAVAILABLE, error),
        _ => error(StatusCode::SERVICE_UNAVAILABLE, "Zed session is not responding"),
    }
}

async fn command(State(context): State<Context>, Json(input): Json<CommandInput>) -> Response {
    if !is_action(&input.command) {
        return error(StatusCode::BAD_REQUEST, "unknown proof command");
    }
    let (reply, result) = flume::bounded(1);
    if context.tx.send(Event::Command { input, reply }).is_err() {
        return error(StatusCode::SERVICE_UNAVAILABLE, "Zed session has stopped");
    }
    match SendWrapper::new(compio::time::timeout(Duration::from_secs(5), result.recv_async())).await {
        Ok(Ok(Ok(()))) => (StatusCode::ACCEPTED, Json(Accepted { accepted: true })).into_response(),
        Ok(Ok(Err(error))) => failure(StatusCode::CONFLICT, error),
        _ => error(StatusCode::SERVICE_UNAVAILABLE, "Zed session is not responding"),
    }
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, content_type)], body).into_response()
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(ErrorBody { error: message })).into_response()
}

fn failure(status: StatusCode, source: crate::Error) -> Response {
    error(status, &snafu::Report::from_error(source).to_string())
}

fn constant_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, StatusCode};

    use super::{Context, authorize};

    #[test]
    fn rejects_mutations_without_authentication_and_foreign_origins() {
        let (tx, _) = flume::unbounded();
        let context = Context::new(1234, "secret", tx);
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("127.0.0.1:1234"));
        assert_eq!(
            authorize(&context, &headers, "/api/command").unwrap_err().status(),
            StatusCode::UNAUTHORIZED,
        );
        headers.insert("origin", HeaderValue::from_static("https://evil.example"));
        headers.insert("authorization", HeaderValue::from_static("Bearer secret"));
        assert_eq!(
            authorize(&context, &headers, "/api/state").unwrap_err().status(),
            StatusCode::FORBIDDEN,
        );
    }

    #[test]
    fn rejects_duplicate_security_headers() {
        let (tx, _) = flume::unbounded();
        let context = Context::new(1234, "secret", tx);
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("127.0.0.1:1234"));
        headers.append("authorization", HeaderValue::from_static("Bearer secret"));
        headers.append("authorization", HeaderValue::from_static("Bearer secret"));
        assert_eq!(
            authorize(&context, &headers, "/api/state").unwrap_err().status(),
            StatusCode::UNAUTHORIZED,
        );
    }
}
