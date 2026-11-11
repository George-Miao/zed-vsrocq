use std::collections::HashMap;

use async_lsp::lsp_types::{InlayHint, Range, error_codes};
use snafu::{ResultExt, ensure};

use crate::{
    Result,
    document::{Document, Position},
    error,
    navigation::{Navigation, Target},
    protocol::{self, Highlights, Response},
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Progress {
    pub(super) checked: Option<Position>,
    pub(super) checking: Option<Position>,
}

impl Progress {
    pub(super) fn snapshot(
        document: &Document,
        navigation: Option<&Navigation>,
        highlights: &Highlights,
        expected: Option<Target>,
        blocked: Option<Position>,
    ) -> Result<Self> {
        for range in highlights
            .prepared_range
            .iter()
            .chain(&highlights.processing_range)
            .chain(&highlights.processed_range)
        {
            ensure!(range.start <= range.end, error::ReversedHighlightRangeSnafu {
                range: *range
            });
            document
                .validate_position(range.start)
                .context(error::HighlightPositionSnafu)?;
            document
                .validate_position(range.end)
                .context(error::HighlightPositionSnafu)?;
        }
        let Some(Target::Sentence(target)) = expected else {
            return Ok(Self::default());
        };
        let checked = navigation
            .map(|navigation| navigation.checked(document, &highlights.processed_range))
            .transpose()?
            .flatten()
            .map(|checked| checked.min(target.end))
            .filter(|checked| blocked.is_none_or(|blocked| *checked <= blocked));
        let checking = blocked
            .is_none()
            .then(|| {
                highlights
                    .processing_range
                    .iter()
                    .filter(|range| range.start < range.end && range.start < target.end)
                    .map(|range| range.end.min(target.end))
                    .min()
            })
            .flatten();
        Ok(Self { checked, checking })
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Identity {
    version: i64,
    revision: u64,
}

struct Frontier {
    epoch: u64,
    progress: Progress,
}

struct OpenDocument {
    identity: Identity,
    frontier: Option<Frontier>,
}

#[derive(Clone)]
pub(super) struct Query {
    uri: String,
    identity: Option<Identity>,
    epoch: Option<u64>,
    range: Range,
}

#[derive(Default)]
pub(super) struct Hints {
    documents: HashMap<String, OpenDocument>,
    revision: u64,
}

impl Hints {
    pub(super) fn open(&mut self, document: &Document) {
        self.revision += 1;
        self.documents.insert(document.uri.clone(), OpenDocument {
            identity: Identity {
                version: document.version,
                revision: self.revision,
            },
            frontier: None,
        });
    }

    pub(super) fn close(&mut self, uri: &str) {
        self.documents.remove(uri);
    }

    pub(super) fn start(&mut self, uri: &str, version: i64, epoch: u64) {
        if let Some(document) = self
            .documents
            .get_mut(uri)
            .filter(|document| document.identity.version == version)
        {
            document.frontier = Some(Frontier {
                epoch,
                progress: Progress::default(),
            });
        }
    }

    pub(super) fn update(&mut self, uri: &str, version: i64, epoch: u64, progress: Progress) -> bool {
        let Some(frontier) = self
            .documents
            .get_mut(uri)
            .filter(|document| document.identity.version == version)
            .and_then(|document| document.frontier.as_mut())
            .filter(|frontier| frontier.epoch == epoch)
        else {
            return false;
        };
        let changed = frontier.progress != progress;
        frontier.progress = progress;
        changed
    }

    pub(super) fn stop_checking(&mut self, uri: &str) -> bool {
        self.documents
            .get_mut(uri)
            .and_then(|document| document.frontier.as_mut())
            .is_some_and(|frontier| frontier.progress.checking.take().is_some())
    }

    pub(super) fn clear(&mut self, uri: &str) {
        if let Some(document) = self.documents.get_mut(uri) {
            document.frontier = None;
        }
    }

    pub(super) fn clear_all(&mut self) {
        for document in self.documents.values_mut() {
            document.frontier = None;
        }
    }

    pub(super) fn query(&self, uri: String, range: Range) -> Query {
        let document = self.documents.get(&uri);
        Query {
            uri,
            identity: document.map(|document| document.identity),
            epoch: document.and_then(|document| document.frontier.as_ref().map(|frontier| frontier.epoch)),
            range,
        }
    }

    fn collect(&self, query: &Query) -> Vec<InlayHint> {
        let progress = self
            .documents
            .get(&query.uri)
            .filter(|document| Some(document.identity) == query.identity)
            .and_then(|document| document.frontier.as_ref())
            .filter(|frontier| Some(frontier.epoch) == query.epoch)
            .map_or(Progress::default(), |frontier| frontier.progress);
        protocol::inlay::hints(progress.checked, progress.checking, query.range)
    }

    pub(super) fn response(&self, query: &Query, mut response: Response) -> protocol::Result<Response> {
        let Ok(native) = &response.result else {
            return Ok(response);
        };
        let identity = self.documents.get(&query.uri).map(|document| document.identity);
        if identity != query.identity {
            return Ok(protocol::failure(
                response.id,
                error_codes::CONTENT_MODIFIED,
                "The inlay hint document changed before VsRocq replied",
            ));
        }
        if let (Some(identity), Some(epoch)) = (query.identity, query.epoch) {
            let local = self.collect(query);
            if !local.is_empty() {
                response.result = Ok(Some(protocol::inlay::merge(
                    native.as_deref(),
                    &local,
                    protocol::inlay::Identity {
                        uri: &query.uri,
                        version: identity.version,
                        epoch,
                    },
                )?));
            }
        }
        Ok(response)
    }

    pub(super) fn resolves(&self, resolved: &protocol::inlay::Resolved) -> bool {
        self.documents
            .get(&resolved.uri)
            .filter(|document| document.identity.version == resolved.version)
            .and_then(|document| document.frontier.as_ref())
            .filter(|frontier| frontier.epoch == resolved.epoch)
            .is_some_and(|frontier| resolved.matches(frontier.progress.checked, frontier.progress.checking))
    }
}

#[derive(Default)]
pub(super) struct Refresh {
    supported: bool,
    pending: bool,
    dirty: bool,
}

impl Refresh {
    pub(super) fn support(&mut self, supported: bool) {
        self.supported = supported;
    }

    pub(super) fn changed(&mut self) {
        self.dirty |= self.supported;
    }

    pub(super) fn request(&mut self) -> bool {
        if !self.supported || self.pending || !self.dirty {
            return false;
        }
        self.pending = true;
        self.dirty = false;
        true
    }

    pub(super) fn complete(&mut self, success: bool) {
        self.pending = false;
        if !success {
            self.supported = false;
            self.dirty = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use async_lsp::lsp_types::{InlayHint, InlayHintLabel, Range, error_codes};

    use super::{Hints, Progress, Refresh};
    use crate::{
        document::{Document, Position},
        navigation::{Navigation, Target},
        protocol::{self, Highlights, Id, Raw, Response},
    };

    fn document(uri: &str, version: i64) -> Document {
        Document {
            uri: uri.into(),
            language: "rocq".into(),
            version,
            text: "Check nat.\nCheck bool.".into(),
        }
    }

    fn range(start: (u32, u32), end: (u32, u32)) -> Range {
        Range::new(Position::new(start.0, start.1), Position::new(end.0, end.1))
    }

    fn response() -> Response {
        protocol::reply(Id::Number(7.into()), &Vec::<InlayHint>::new()).unwrap()
    }

    fn labels(response: &Response) -> Vec<(Position, String)> {
        protocol::result::<Vec<InlayHint>>(response)
            .unwrap()
            .into_iter()
            .map(|hint| {
                assert!(hint.kind.is_none());
                let InlayHintLabel::String(label) = hint.label else {
                    panic!("bridge labels are plain text");
                };
                (hint.position, label)
            })
            .collect()
    }

    #[test]
    fn inlay_responses_use_the_requested_document_range_and_latest_progress() {
        let a = document("file:///a.v", 4);
        let b = document("file:///b.v", 4);
        let mut hints = Hints::default();
        hints.open(&a);
        hints.open(&b);
        hints.start(&a.uri, 4, 1);
        hints.update(&a.uri, 4, 1, Progress {
            checked: Some(Position::new(0, 10)),
            checking: Some(Position::new(1, 11)),
        });
        let query = hints.query(a.uri.clone(), range((0, 10), (0, 10)));
        assert_eq!(labels(&hints.response(&query, response()).unwrap()), [(
            Position::new(0, 10),
            "Checked to here".into()
        )]);
        let other = hints.query(b.uri.clone(), range((0, 0), (1, 11)));
        assert!(labels(&hints.response(&other, response()).unwrap()).is_empty());
        let query = hints.query(a.uri.clone(), range((0, 0), (1, 11)));
        hints.update(&a.uri, 4, 1, Progress {
            checked: Some(Position::new(1, 11)),
            checking: None,
        });
        assert_eq!(labels(&hints.response(&query, response()).unwrap()), [(
            Position::new(1, 11),
            "Checked to here".into()
        )]);
    }

    #[test]
    fn edited_closed_and_reopened_documents_reject_pending_native_results() {
        for change in ["edit", "close", "reopen"] {
            let document = document("file:///proof.v", 8);
            let mut hints = Hints::default();
            hints.open(&document);
            hints.start(&document.uri, 8, 3);
            hints.update(&document.uri, 8, 3, Progress {
                checked: Some(Position::new(0, 10)),
                checking: None,
            });
            let query = hints.query(document.uri.clone(), range((0, 0), (1, 11)));
            match change {
                "edit" => hints.open(&super::tests::document(&document.uri, 9)),
                "close" => hints.close(&document.uri),
                "reopen" => {
                    hints.close(&document.uri);
                    hints.open(&document);
                }
                _ => unreachable!(),
            }
            let error = hints.response(&query, response()).unwrap().result.unwrap_err();
            assert_eq!(error.code, error_codes::CONTENT_MODIFIED);
            let current = hints.query(document.uri.clone(), range((0, 0), (1, 11)));
            assert!(labels(&hints.response(&current, response()).unwrap()).is_empty());
        }
    }

    #[test]
    fn reset_and_replaced_epochs_never_resurrect_frontier_hints() {
        let document = document("file:///proof.v", 8);
        let mut hints = Hints::default();
        hints.open(&document);
        hints.start(&document.uri, 8, 3);
        let query = hints.query(document.uri.clone(), range((0, 0), (1, 11)));
        hints.update(&document.uri, 8, 3, Progress {
            checked: Some(Position::new(0, 10)),
            checking: None,
        });
        hints.clear(&document.uri);
        assert!(labels(&hints.response(&query, response()).unwrap()).is_empty());
        hints.start(&document.uri, 8, 4);
        assert!(!hints.update(&document.uri, 8, 3, Progress {
            checked: Some(Position::new(1, 11)),
            checking: None,
        }));
        assert!(!hints.update(&document.uri, 7, 4, Progress {
            checked: Some(Position::new(1, 11)),
            checking: None,
        }));
        hints.update(&document.uri, 8, 4, Progress {
            checked: None,
            checking: Some(Position::new(1, 11)),
        });
        assert!(labels(&hints.response(&query, response()).unwrap()).is_empty());
        let current = hints.query(document.uri.clone(), range((0, 0), (1, 11)));
        assert_eq!(labels(&hints.response(&current, response()).unwrap()), [(
            Position::new(1, 11),
            "Checking...".into()
        )]);
        hints.clear_all();
        assert!(labels(&hints.response(&current, response()).unwrap()).is_empty());
    }

    #[test]
    fn native_hints_and_errors_keep_opaque_members_and_error_data() {
        let document = document("file:///proof.v", 1);
        let mut hints = Hints::default();
        hints.open(&document);
        hints.start(&document.uri, 1, 2);
        hints.update(&document.uri, 1, 2, Progress {
            checked: Some(Position::new(0, 10)),
            checking: None,
        });
        let query = hints.query(document.uri.clone(), range((0, 0), (1, 11)));
        let native: Raw = serde_json::from_str(r#"[{"position":{"line":0,"character":2},"label":"native","data":{"number":1.20e+3},"vendor":[null,true]}]"#).unwrap();
        let merged = hints
            .response(&query, Response {
                id: Id::Number(7.into()),
                result: Ok(Some(native)),
            })
            .unwrap();
        let values: Vec<Raw> = protocol::result(&merged).unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(
            values[0].get(),
            r#"{"position":{"line":0,"character":2},"label":"native","data":{"number":1.20e+3},"vendor":[null,true]}"#
        );
        let error = protocol::Error {
            code: -32803,
            message: "native rejected".into(),
            data: Some(serde_json::from_str(r#"{"reason":[null,1.20e+3]}"#).unwrap()),
        };
        let expected = serde_json::to_string(&error).unwrap();
        hints.close(&document.uri);
        let failed = hints
            .response(&query, Response {
                id: Id::Number(7.into()),
                result: Err(error),
            })
            .unwrap();
        assert_eq!(serde_json::to_string(&failed.result.unwrap_err()).unwrap(), expected);
    }

    #[test]
    fn refreshes_coalesce_while_outstanding_and_rejection_disables_only_refresh() {
        let mut refresh = Refresh::default();
        refresh.changed();
        assert!(!refresh.request());
        refresh.support(true);
        refresh.changed();
        assert!(refresh.request());
        for _ in 0..5 {
            refresh.changed();
            assert!(!refresh.request());
        }
        refresh.complete(true);
        assert!(refresh.request());
        refresh.complete(true);
        assert!(!refresh.request());
        refresh.changed();
        assert!(refresh.request());
        refresh.changed();
        refresh.complete(false);
        refresh.changed();
        assert!(!refresh.request());
    }

    #[test]
    fn real_highlight_snapshots_distinguish_prepared_processing_checked_and_failed_work() {
        let document = document("file:///proof.v", 4);
        let rows = "[1] [Check--nat--.] (0 -> 10) (not executed)\n[2] [Check--bool--.] (11 -> 22) (not executed)";
        let dump =
            format!("Document using sentences_by_id map\n\n{rows}\n\nDocument using sentences_by_end map\n\n{rows}");
        let navigation = Navigation::parse(&document, &dump).unwrap();
        let first = range((0, 0), (0, 10));
        let second = range((1, 0), (1, 11));
        let expected = Some(Target::Sentence(second));
        let mut highlights = Highlights {
            uri: document.uri.clone(),
            prepared_range: vec![second],
            processing_range: Vec::new(),
            processed_range: Vec::new(),
        };
        let snapshot = |highlights: &Highlights, blocked| {
            Progress::snapshot(&document, Some(&navigation), highlights, expected, blocked).unwrap()
        };
        assert_eq!(snapshot(&highlights, None), Progress::default());
        highlights.processing_range = vec![first];
        assert_eq!(snapshot(&highlights, None), Progress {
            checked: None,
            checking: Some(Position::new(0, 10)),
        });
        highlights.processing_range.clear();
        highlights.processed_range = vec![second];
        assert_eq!(snapshot(&highlights, None), Progress::default());
        highlights.processing_range = vec![second];
        highlights.processed_range = vec![first];
        let mut hints = Hints::default();
        hints.open(&document);
        hints.start(&document.uri, 4, 3);
        hints.update(&document.uri, 4, 3, snapshot(&highlights, None));
        let query = hints.query(document.uri.clone(), range((0, 0), (1, 11)));
        assert_eq!(labels(&hints.response(&query, response()).unwrap()), [
            (Position::new(0, 10), "Checked to here".into()),
            (Position::new(1, 11), "Checking...".into()),
        ]);
        hints.update(&document.uri, 4, 3, snapshot(&highlights, Some(second.start)));
        assert_eq!(labels(&hints.response(&query, response()).unwrap()), [(
            Position::new(0, 10),
            "Checked to here".into()
        ),]);
        highlights.processed_range = vec![range((0, 0), (1, 11))];
        assert_eq!(
            Progress::snapshot(
                &document,
                Some(&navigation),
                &highlights,
                Some(Target::Sentence(first)),
                None,
            )
            .unwrap()
            .checked,
            Some(first.end)
        );
        assert_eq!(
            Progress::snapshot(&document, Some(&navigation), &highlights, Some(Target::Top), None,).unwrap(),
            Progress::default()
        );
    }
}
