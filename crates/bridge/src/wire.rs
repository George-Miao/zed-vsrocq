use std::{collections::VecDeque, io};

use compio::{BufResult, io::AsyncRead};
use serde::Serialize;
use snafu::{ResultExt, Snafu};
use weavatrix_lsp_wire::{Decoder, Encoder, Limits};

use crate::protocol::Message;

pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Snafu)]
pub(crate) enum Error {
    #[snafu(display("cannot read LSP input: {source}"))]
    ReadInput { source: io::Error },

    #[snafu(display("cannot decode LSP frame: {source}"))]
    DecodeFrame { source: weavatrix_lsp_wire::DecodeError },

    #[snafu(display("cannot decode LSP message: {source}"))]
    DecodeMessage { source: serde_json::Error },

    #[snafu(display("cannot serialize LSP message: {source}"))]
    EncodeMessage { source: serde_json::Error },

    #[snafu(display("cannot encode LSP frame: {source}"))]
    EncodeFrame { source: weavatrix_lsp_wire::EncodeError },
}

pub(crate) struct Reader<R> {
    input: R,
    decoder: Decoder,
    pending: VecDeque<Result<Message>>,
    buffer: Vec<u8>,
    ended: bool,
}

impl<R: AsyncRead> Reader<R> {
    pub(crate) fn new(input: R) -> Self {
        Self {
            input,
            decoder: Decoder::new(limits()),
            pending: VecDeque::new(),
            buffer: Vec::with_capacity(8192),
            ended: false,
        }
    }

    pub(crate) async fn read(&mut self) -> Result<Option<Message>> {
        loop {
            if let Some(message) = self.pending.pop_front() {
                return message.map(Some);
            }
            if self.ended {
                return Ok(None);
            }
            self.buffer.clear();
            let BufResult(result, buffer) = self.input.read(std::mem::take(&mut self.buffer)).await;
            self.buffer = buffer;
            let length = result.context(ReadInputSnafu)?;
            if length == 0 {
                self.decoder.finish().context(DecodeFrameSnafu)?;
                self.ended = true;
                return Ok(None);
            }
            let pending = &mut self.pending;
            self.decoder
                .push(&self.buffer[..length], |body| {
                    pending.push_back(serde_json::from_slice(body).context(DecodeMessageSnafu));
                })
                .context(DecodeFrameSnafu)?;
        }
    }
}

pub(crate) fn encode<T: Serialize + ?Sized>(message: &T) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(message).context(EncodeMessageSnafu)?;
    Encoder::new(limits()).encode_owned(body).context(EncodeFrameSnafu)
}

fn limits() -> Limits {
    Limits::new(16 * 1024, 16 * 1024, 32, 64 * 1024 * 1024, 64 * 1024 * 1024)
        .expect("the fixed LSP framing limits are internally consistent")
}

#[cfg(test)]
mod tests {
    use serde_json::value::RawValue;

    use super::{Error, Reader, encode};
    use crate::protocol::Message;

    fn frame(json: &str) -> Vec<u8> {
        let raw = RawValue::from_string(json.to_owned()).unwrap();
        encode(raw.as_ref()).unwrap()
    }

    #[test]
    fn frames_unicode_by_byte_length_and_keeps_following_frame() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let first_params = "{\"text\":\"\u{1f9a4} : forall\"}";
            let mut bytes = frame(&format!(
                r#"{{"jsonrpc":"2.0","method":"first","params":{first_params}}}"#
            ));
            bytes.extend(frame(r#"{"jsonrpc":"2.0","method":"next","params":{"next":true}}"#));
            let mut input = Reader::new(bytes.as_slice());
            let Some(Message::Request(first)) = input.read().await.unwrap() else {
                panic!("the first frame must be a notification");
            };
            assert_eq!(first.method, "first");
            assert!(first.id.is_none());
            assert_eq!(first.params.unwrap().get(), first_params);
            let Some(Message::Request(next)) = input.read().await.unwrap() else {
                panic!("the second frame must be a notification");
            };
            assert_eq!(next.method, "next");
            assert_eq!(next.params.unwrap().get(), r#"{"next":true}"#);
            assert!(input.read().await.unwrap().is_none());
        });
    }

    #[test]
    fn rejects_ambiguous_lengths() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let mut input = Reader::new(&b"Content-Length: 2\r\nContent-Length: 3\r\n\r\n{}"[..]);
            assert!(matches!(
                input.read().await,
                Err(Error::DecodeFrame {
                    source: weavatrix_lsp_wire::DecodeError::DuplicateContentLength,
                })
            ));
        });
    }

    #[test]
    fn preserves_signed_string_and_large_server_request_identifiers() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            for id in [
                "-7",
                r#""server-request""#,
                "4611686018427387903",
                "18446744073709551615",
            ] {
                let json = format!(
                    r#"{{"jsonrpc":"2.0","id":{id},"method":"workspace/configuration","params":{{"items":[]}}}}"#
                );
                let message: Message = serde_json::from_str(&json).unwrap();
                let bytes = encode(&message).unwrap();
                let Some(Message::Request(request)) = Reader::new(bytes.as_slice()).read().await.unwrap() else {
                    panic!("the frame must be a server request");
                };
                assert_eq!(serde_json::to_string(&request.id.unwrap()).unwrap(), id);
                assert_eq!(request.method, "workspace/configuration");
                assert_eq!(request.params.unwrap().get(), r#"{"items":[]}"#);
            }
        });
    }

    #[test]
    fn absent_params_stays_absent() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let bytes = frame(r#"{"jsonrpc":"2.0","method":"initialized"}"#);
            let Some(Message::Request(request)) = Reader::new(bytes.as_slice()).read().await.unwrap() else {
                panic!("the frame must be a notification");
            };
            assert!(request.params.is_none());
            assert!(!serde_json::to_string(&request).unwrap().contains("params"));
        });
    }

    #[test]
    fn preserves_unknown_params_and_results_as_raw_json() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let params = r#"{ "extension": [null, {"number": 1.00e+2}], "command": ["opaque", false] }"#;
            let bytes = frame(&format!(
                r#"{{"jsonrpc":"2.0","id":"extension","method":"custom/request","params":{params}}}"#
            ));
            let Some(Message::Request(request)) = Reader::new(bytes.as_slice()).read().await.unwrap() else {
                panic!("the frame must be a server request");
            };
            assert_eq!(request.params.unwrap().get(), params);

            let result = r#"[ { "extension": 1.00e+2 }, null, "opaque" ]"#;
            let bytes = frame(&format!(r#"{{"jsonrpc":"2.0","id":-7,"result":{result}}}"#));
            let Some(Message::Response(response)) = Reader::new(bytes.as_slice()).read().await.unwrap() else {
                panic!("the frame must be a response");
            };
            assert_eq!(response.result.unwrap().unwrap().get(), result);
        });
    }

    #[test]
    fn preserves_null_success_results() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let bytes = frame(r#"{"jsonrpc":"2.0","id":"shutdown","result":null}"#);
            let Some(Message::Response(response)) = Reader::new(bytes.as_slice()).read().await.unwrap() else {
                panic!("the frame must be a response");
            };
            assert!(response.result.as_ref().unwrap().is_none());
            assert!(serde_json::to_string(&response).unwrap().contains(r#""result":null"#));
        });
    }

    #[test]
    fn rejects_json_without_a_valid_rpc_envelope() {
        compio::runtime::Runtime::new().unwrap().block_on(async {
            let bytes = frame(r#"{"text":"not an RPC message"}"#);
            assert!(matches!(
                Reader::new(bytes.as_slice()).read().await,
                Err(Error::DecodeMessage { .. })
            ));
        });
    }
}
