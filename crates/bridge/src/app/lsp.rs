use std::{
    future::Future,
    ops::ControlFlow,
    pin::Pin,
    task::{Context, Poll},
};

use async_lsp::{AnyEvent, AnyNotification, AnyRequest, ErrorCode, LspService, ResponseError};
use serde_json::Value;
use snafu::{ResultExt, Snafu};

use super::{App, Event};
use crate::{
    Result,
    protocol::{self, Id, Message, Raw, Request, Response},
};

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(super)))]
pub(crate) enum Error {
    #[snafu(display("cannot encode editor notification: {source}"))]
    EncodeNotification { source: serde_json::Error },

    #[snafu(display("cannot decode editor notification: {source}"))]
    DecodeNotification { source: serde_json::Error },

    #[snafu(display("cannot send editor notification: {source}"))]
    SendNotification { source: async_lsp::Error },

    #[snafu(display("cannot encode editor request: {source}"))]
    EncodeRequest { source: serde_json::Error },

    #[snafu(display("cannot decode editor request: {source}"))]
    DecodeRequest { source: serde_json::Error },

    #[snafu(display("cannot encode editor error response: {source}"))]
    EncodeErrorResponse { source: serde_json::Error },

    #[snafu(display("cannot decode editor error response: {source}"))]
    DecodeErrorResponse { source: serde_json::Error },

    #[snafu(display("native error code {code} does not fit an LSP integer: {source}"))]
    ErrorCodeOverflow {
        code: i64,
        source: std::num::TryFromIntError,
    },

    #[snafu(display("VsRocq session stopped before replying: {source}"))]
    ReplyStopped { source: flume::RecvError },
}

#[derive(Debug, Snafu)]
pub(super) enum ReplyError {
    #[snafu(context(false), display("{source}"))]
    Bridge { source: crate::Error },

    #[snafu(context(false), display("{source}"))]
    Native { source: protocol::Error },
}

pub(super) type Reply = std::result::Result<Option<Raw>, ReplyError>;

impl tower_service::Service<AnyRequest> for App {
    type Error = ResponseError;
    type Future = Pin<Box<dyn Future<Output = async_lsp::Result<Value, ResponseError>> + Send>>;
    type Response = Value;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<async_lsp::Result<(), ResponseError>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: AnyRequest) -> Self::Future {
        let id = match request.id {
            async_lsp::lsp_types::NumberOrString::Number(number) => Id::Number(number.into()),
            async_lsp::lsp_types::NumberOrString::String(string) => Id::String(string),
        };
        let (reply, response) = flume::bounded(1);
        self.replies.insert(id.clone(), reply);
        let result = incoming(request.method, request.params, Some(id.clone()))
            .and_then(|request| self.client(Message::Request(request)));
        if let Err(error) = result {
            if let Some(reply) = self.replies.remove(&id) {
                let _ = reply.try_send(Err(error.into()));
            }
        }
        Box::pin(async move {
            match response.recv_async().await {
                Ok(Ok(result)) => {
                    serde_json::from_str(result.as_deref().map_or("null", serde_json::value::RawValue::get))
                        .map_err(|error| ResponseError::new(ErrorCode::INTERNAL_ERROR, error))
                }
                Ok(Err(ReplyError::Native { source })) => Err(framework_error(&source).unwrap_or_else(|error| {
                    ResponseError::new(ErrorCode::INTERNAL_ERROR, snafu::Report::from_error(error))
                })),
                Ok(Err(ReplyError::Bridge { source })) => Err(ResponseError::new(
                    ErrorCode::REQUEST_FAILED,
                    snafu::Report::from_error(source),
                )),
                Err(source) => Err(ResponseError::new(ErrorCode::REQUEST_FAILED, Error::ReplyStopped {
                    source,
                })),
            }
        })
    }
}

impl LspService for App {
    fn notify(&mut self, notification: AnyNotification) -> ControlFlow<async_lsp::Result<()>> {
        if notification.method == "exit" {
            return ControlFlow::Break(Ok(()));
        }
        let result = incoming(notification.method, notification.params, None)
            .and_then(|request| self.client(Message::Request(request)));
        match result {
            Ok(()) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(Err(async_lsp::Error::Protocol(error.to_string()))),
        }
    }

    fn emit(&mut self, event: AnyEvent) -> ControlFlow<async_lsp::Result<()>> {
        let event = match event.downcast::<Event>() {
            Ok(event) => event,
            Err(event) => {
                return ControlFlow::Break(Err(async_lsp::Error::Routing(format!(
                    "unknown bridge event: {event:?}"
                ))));
            }
        };
        match self.event(event) {
            Ok(()) => ControlFlow::Continue(()),
            Err(error) => ControlFlow::Break(Err(async_lsp::Error::Protocol(error.to_string()))),
        }
    }
}

impl App {
    pub(super) fn output(&mut self, message: Message) -> Result<()> {
        match message {
            Message::Request(mut request) => {
                if let Some(id) = request.id.take() {
                    let request = request_to_client(request)?;
                    let request = tower_service::Service::call(&mut self.socket, request);
                    let tx = self.tx.clone();
                    compio::runtime::spawn(async move {
                        let result = request.await;
                        let _ = tx.try_send(Event::EditorResponse { id, result });
                    })
                    .detach();
                    return Ok(());
                }
                let bytes = serde_json::to_vec(&request).context(EncodeNotificationSnafu)?;
                let notification: AnyNotification = serde_json::from_slice(&bytes).context(DecodeNotificationSnafu)?;
                match LspService::notify(&mut self.socket, notification) {
                    ControlFlow::Continue(()) => Ok(()),
                    ControlFlow::Break(result) => Ok(result.context(SendNotificationSnafu)?),
                }
            }
            Message::Response(response) => complete_response(response, &mut self.replies),
        }
    }
}

fn incoming(method: String, params: Value, id: Option<Id>) -> Result<Request> {
    // async-lsp represents absent params as null. Keep absence at the native
    // transport boundary instead of forwarding an explicit params:null.
    let params = (!params.is_null()).then(|| protocol::raw(&params)).transpose()?;
    Ok(Request { method, params, id })
}

fn request_to_client(mut request: Request) -> Result<AnyRequest> {
    // MainLoop allocates the editor-facing ID. Backend IDs may be OCaml max_int
    // or signed integers, and must never pass through async-lsp's i32 ID type.
    request.id = Some(Id::Number(0.into()));
    let bytes = serde_json::to_vec(&request).context(EncodeRequestSnafu)?;
    Ok(serde_json::from_slice(&bytes).context(DecodeRequestSnafu)?)
}

pub(super) fn native_response(id: Id, result: std::result::Result<Value, ResponseError>) -> Response {
    let result = match result {
        Ok(result) => protocol::raw(&result).map(Some).map_err(|error| protocol::Error {
            code: i64::from(ErrorCode::INTERNAL_ERROR.0),
            message: error.to_string(),
            data: None,
        }),
        Err(error) => native_error(error),
    };
    Response { id, result }
}

fn native_error(error: ResponseError) -> protocol::Reply {
    let data = match error.data.as_ref().map(protocol::raw).transpose() {
        Ok(data) => data,
        Err(error) => {
            return Err(protocol::Error {
                code: i64::from(ErrorCode::INTERNAL_ERROR.0),
                message: error.to_string(),
                data: None,
            });
        }
    };
    Err(protocol::Error {
        code: i64::from(error.code.0),
        message: error.message,
        data,
    })
}

fn framework_error(error: &protocol::Error) -> Result<ResponseError> {
    // Deserialize through the framework's public serde interface, including its
    // checked i64-to-i32 error-code conversion and opaque error data.
    let bytes = serde_json::to_vec(error).context(EncodeErrorResponseSnafu)?;
    Ok(serde_json::from_slice(&bytes).context(DecodeErrorResponseSnafu)?)
}

fn complete_response(
    response: Response,
    replies: &mut std::collections::HashMap<Id, flume::Sender<Reply>>,
) -> Result<()> {
    let Some(reply) = replies.remove(&response.id) else {
        return Ok(());
    };
    if let Err(error) = &response.result {
        i32::try_from(error.code).context(ErrorCodeOverflowSnafu { code: error.code })?;
    }
    let _ = reply.try_send(response.result.map_err(ReplyError::from));
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{ReplyError, complete_response};
    use crate::protocol::{self, Id, Raw, Response};

    #[test]
    fn forwarded_native_results_match_exact_request_identity() {
        for payload in ["null", r#"{"native":{"applied":true,"explanation":["a","b"]}}"#] {
            let (reply, response) = flume::bounded(1);
            let mut replies = HashMap::from([(Id::Number(42.into()), reply)]);
            complete_response(
                protocol::reply(Id::String("42".into()), &"wrong request").unwrap(),
                &mut replies,
            )
            .unwrap();
            assert!(response.try_recv().is_err());
            let result = serde_json::from_str::<Option<Raw>>(payload).unwrap();
            complete_response(
                Response {
                    id: Id::Number(42.into()),
                    result: Ok(result),
                },
                &mut replies,
            )
            .unwrap();
            assert_eq!(
                serde_json::to_string(&response.try_recv().unwrap().unwrap()).unwrap(),
                payload
            );
        }
    }

    #[test]
    fn forwarded_native_command_errors_keep_their_code_message_and_data() {
        let (reply, response) = flume::bounded(1);
        let id = Id::String("native-command".into());
        let mut replies = HashMap::from([(id.clone(), reply)]);
        let error: protocol::Error = serde_json::from_str(r#"{"code":-32803,"message":"Native execution failed","data":{"reason":"proof rejected","uri":"file:///proof.v"}}"#).unwrap();
        let expected = serde_json::to_string(&error).unwrap();
        complete_response(Response { id, result: Err(error) }, &mut replies).unwrap();
        let ReplyError::Native { source: error } = response.try_recv().unwrap().unwrap_err() else {
            panic!("a native failure must keep the original protocol error");
        };
        assert_eq!(serde_json::to_string(&error).unwrap(), expected);
    }
}
