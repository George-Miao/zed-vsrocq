use std::{collections::HashSet, num::ParseIntError, sync::LazyLock};

pub(crate) use async_lsp::lsp_types::Range;
use regex::Regex;
use snafu::{OptionExt, ResultExt, Snafu, ensure};

use crate::document::{self, Document, Position};

#[derive(Debug, Snafu)]
pub(crate) enum Error {
    #[snafu(display("VsRocq documentState has an unknown header"))]
    UnknownHeader,

    #[snafu(display("VsRocq documentState is missing its second sentence map"))]
    MissingSentenceMap,

    #[snafu(display("VsRocq documentState sentence maps disagree"))]
    SentenceMapMismatch,

    #[snafu(display("run-to-cursor is missing its position"))]
    MissingCursorPosition,

    #[snafu(display("unsupported proof navigation command: {action}"))]
    UnsupportedCommand { action: String },

    #[snafu(display("cannot resolve the proof navigation cursor position"))]
    CursorPosition { source: document::Error },

    #[snafu(display("cannot resolve the range of VsRocq sentence {sentence}"))]
    SentencePosition { sentence: i64, source: document::Error },

    #[snafu(display("cannot resolve the VsRocq block-on-error position"))]
    BlockedPosition { source: document::Error },

    #[snafu(display("VsRocq block-on-error range {range:?} is outside its sentence map"))]
    BlockedRangeOutsideMap { range: Range },

    #[snafu(display("VsRocq documentState has an unknown row format"))]
    UnknownRowFormat,

    #[snafu(display("VsRocq documentState row was not captured"))]
    MissingRowCapture,

    #[snafu(display("VsRocq parsing-error offset {value} is out of range"))]
    ParsingErrorStart { value: String, source: ParseIntError },

    #[snafu(display("VsRocq sentence ID {value} is out of range"))]
    SentenceId { value: String, source: ParseIntError },

    #[snafu(display("VsRocq sentence {sentence} is missing its start"))]
    MissingSentenceStart { sentence: i64 },

    #[snafu(display("VsRocq sentence {sentence} has an invalid start"))]
    SentenceStart { sentence: i64, source: ParseIntError },

    #[snafu(display("VsRocq sentence {sentence} is missing its end"))]
    MissingSentenceEnd { sentence: i64 },

    #[snafu(display("VsRocq sentence {sentence} has an invalid end"))]
    SentenceEnd { sentence: i64, source: ParseIntError },

    #[snafu(display(
        "VsRocq sentence {sentence} has an invalid byte range {start}..{stop} for a document of {length} bytes"
    ))]
    InvalidSentenceRange {
        sentence: i64,
        start: usize,
        stop: usize,
        length: usize,
    },

    #[snafu(display("VsRocq documentState repeats sentence ID {sentence}"))]
    DuplicateSentence { sentence: i64 },

    #[snafu(display("VsRocq sentence {sentence} overlaps sentence {previous}"))]
    OverlappingSentences { previous: i64, sentence: i64 },
}

pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Target {
    Top,
    Sentence(Range),
}

#[derive(Debug, Eq, PartialEq)]
struct Sentence {
    id: i64,
    start: usize,
    stop: usize,
}

pub(crate) struct Navigation {
    sentences: Vec<Sentence>,
    current: Option<usize>,
    parse_error: Option<usize>,
}

impl Navigation {
    // VsRocq does not expose every sentence through structured symbol/proof
    // queries. Keep its debug document-state format dependency confined to this
    // adapter.
    pub(crate) fn parse(document: &Document, state: &str) -> Result<Self> {
        let state = state
            .strip_prefix("Document using sentences_by_id map\n")
            .context(UnknownHeaderSnafu)?;
        let (by_id, by_end) = state
            .split_once("\nDocument using sentences_by_end map\n")
            .context(MissingSentenceMapSnafu)?;
        let (sentences, parse_error) = parse_map(document, by_id)?;
        let (other_sentences, other_error) = parse_map(document, by_end)?;
        ensure!(
            sentences == other_sentences && parse_error == other_error,
            SentenceMapMismatchSnafu
        );
        Ok(Self {
            sentences,
            current: None,
            parse_error,
        })
    }

    pub(crate) fn target(&mut self, document: &Document, action: &str, position: Option<Position>) -> Result<Target> {
        let last = self.sentences.len().checked_sub(1);
        self.current = match action {
            "step-forward" => last.map(|last| self.current.map_or(0, |current| current.saturating_add(1).min(last))),
            "step-backward" => self.current.and_then(|current| current.checked_sub(1)),
            "run-to-end" => last,
            "run-to-cursor" => {
                let position = document
                    .byte_offset(position.context(MissingCursorPositionSnafu)?)
                    .context(CursorPositionSnafu)?;
                self.sentences.iter().rposition(|sentence| sentence.stop <= position)
            }
            _ => return UnsupportedCommandSnafu { action }.fail(),
        };
        match self.current {
            None => Ok(Target::Top),
            Some(current) => {
                let sentence = &self.sentences[current];
                Ok(Target::Sentence(Range {
                    start: document
                        .position_at(sentence.start)
                        .context(SentencePositionSnafu { sentence: sentence.id })?,
                    end: document
                        .position_at(sentence.stop)
                        .context(SentencePositionSnafu { sentence: sentence.id })?,
                }))
            }
        }
    }

    pub(crate) fn checked(&self, document: &Document, processed: &[Range]) -> Result<Option<Position>> {
        let mut checked = None;
        for sentence in &self.sentences {
            if self.parse_error.is_some_and(|error| sentence.stop > error) {
                break;
            }
            let start = document
                .position_at(sentence.start)
                .context(SentencePositionSnafu { sentence: sentence.id })?;
            let end = document
                .position_at(sentence.stop)
                .context(SentencePositionSnafu { sentence: sentence.id })?;
            if !processed.iter().any(|range| range.start <= start && end <= range.end) {
                break;
            }
            checked = Some(end);
        }
        Ok(checked)
    }

    pub(crate) fn blocked(&mut self, document: &Document, range: Range) -> Result<()> {
        let stop = document.byte_offset(range.end).context(BlockedPositionSnafu)?;
        let Some(current) = self.current else {
            return Ok(());
        };
        if stop > self.sentences[current].stop {
            return Ok(());
        }
        let failed = self
            .sentences
            .iter()
            .position(|sentence| sentence.stop >= stop)
            .context(BlockedRangeOutsideMapSnafu { range })?;
        self.current = failed.checked_sub(1);
        Ok(())
    }
}

fn parse_map(document: &Document, mut text: &str) -> Result<(Vec<Sentence>, Option<usize>)> {
    static ROW: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
        r"(?s)\A(?:\[(?P<id>[0-9]+)\] .*? \((?P<start>[0-9]+) -> (?P<stop>[0-9]+)\) \((?:executed(?: in worker)?|not executed)\)|\(\* comment \*\) \(comment\)|\[parsing error\] .*? \((?P<error_start>[0-9]+) -> [0-9]+\) \(error\))(?:\n|\z)"
    ).expect("the fixed document-state row pattern is valid")
    });
    let mut sentences: Vec<Sentence> = Vec::new();
    let mut ids = HashSet::new();
    let mut parse_error = None;
    loop {
        text = text.trim_start_matches('\n');
        if text.is_empty() {
            return Ok((sentences, parse_error));
        }
        let captures = ROW.captures(text).context(UnknownRowFormatSnafu)?;
        let whole = captures.get(0).context(MissingRowCaptureSnafu)?;
        if let Some(start) = captures.name("error_start") {
            let start = start
                .as_str()
                .parse::<usize>()
                .context(ParsingErrorStartSnafu { value: start.as_str() })?;
            parse_error = Some(parse_error.map_or(start, |earlier: usize| earlier.min(start)));
        }
        if let Some(id) = captures.name("id") {
            let id = id
                .as_str()
                .parse::<i64>()
                .context(SentenceIdSnafu { value: id.as_str() })?;
            let start = captures
                .name("start")
                .context(MissingSentenceStartSnafu { sentence: id })?
                .as_str()
                .parse::<usize>()
                .context(SentenceStartSnafu { sentence: id })?;
            let stop = captures
                .name("stop")
                .context(MissingSentenceEndSnafu { sentence: id })?
                .as_str()
                .parse::<usize>()
                .context(SentenceEndSnafu { sentence: id })?;
            ensure!(
                start < stop
                    && stop <= document.text.len()
                    && document.text.is_char_boundary(start)
                    && document.text.is_char_boundary(stop),
                InvalidSentenceRangeSnafu {
                    sentence: id,
                    start,
                    stop,
                    length: document.text.len(),
                }
            );
            ensure!(ids.insert(id), DuplicateSentenceSnafu { sentence: id });
            if let Some(previous) = sentences.last() {
                ensure!(previous.stop <= start, OverlappingSentencesSnafu {
                    previous: previous.id,
                    sentence: id,
                });
            }
            sentences.push(Sentence { id, start, stop });
        }
        text = &text[whole.end()..];
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, Navigation, Target};
    use crate::document::{Document, Position};

    fn document(text: &str) -> Document {
        Document {
            uri: "file:///proof.v".into(),
            language: "rocq".into(),
            version: 1,
            text: text.into(),
        }
    }

    fn dump(rows: &str) -> String {
        format!("Document using sentences_by_id map\n\n{rows}\n\nDocument using sentences_by_end map\n\n{rows}")
    }

    #[test]
    fn empty_and_comment_only_documents_have_a_real_top_target() {
        for (text, rows) in [("", ""), ("(* Nothing to execute. *)", "(* comment *) (comment)")] {
            let doc = document(text);
            let mut navigation = Navigation::parse(&doc, &dump(rows)).unwrap();
            for action in ["run-to-end", "step-forward", "step-backward"] {
                assert_eq!(navigation.target(&doc, action, None).unwrap(), Target::Top);
            }
        }
    }

    #[test]
    fn step_at_eof_rechecks_the_real_last_sentence_instead_of_waiting_for_a_noop() {
        let doc = document("Check nat.\n(* trailing comment *)");
        let mut navigation = Navigation::parse(
            &doc,
            &dump("[7] [Check--nat--.] (0 -> 10) (not executed)\n(* comment *) (comment)"),
        )
        .unwrap();
        let target = navigation.target(&doc, "run-to-end", None).unwrap();
        assert_eq!(navigation.target(&doc, "step-forward", None).unwrap(), target);
        assert!(matches!(target, Target::Sentence(range) if range.end == (Position { line: 0, character: 10 })));
        assert_eq!(navigation.target(&doc, "step-backward", None).unwrap(), Target::Top);
        assert_eq!(navigation.target(&doc, "step-backward", None).unwrap(), Target::Top);
    }

    #[test]
    fn server_byte_ranges_become_utf16_targets() {
        let doc = document("Check \"\u{1f9a4}\".");
        let mut navigation = Navigation::parse(&doc, &dump("[1] [Check--string--.] (0 -> 13) (executed)")).unwrap();
        let target = navigation.target(&doc, "step-forward", None).unwrap();
        assert!(matches!(target, Target::Sentence(range) if range.end == (Position { line: 0, character: 11 })));
        assert_eq!(
            navigation
                .target(&doc, "run-to-cursor", Some(Position { line: 0, character: 0 }))
                .unwrap(),
            Target::Top
        );
    }

    #[test]
    fn stop_on_error_moves_navigation_before_the_failed_sentence() {
        let doc = document("Check nat.\nCheck bool.");
        let rows = "[1] [Check--nat--.] (0 -> 10) (executed)\n[2] [Check--bool--.] (11 -> 22) (not executed)";
        let mut blocked = Navigation::parse(&doc, &dump(rows)).unwrap();
        blocked.target(&doc, "run-to-end", None).unwrap();
        blocked
            .blocked(&doc, super::Range {
                start: Position { line: 1, character: 0 },
                end: Position { line: 1, character: 11 },
            })
            .unwrap();
        assert_eq!(blocked.target(&doc, "step-backward", None).unwrap(), Target::Top);

        let mut continuing = Navigation::parse(&doc, &dump(rows)).unwrap();
        continuing.target(&doc, "run-to-end", None).unwrap();
        let target = continuing.target(&doc, "step-backward", None).unwrap();
        assert!(matches!(target, Target::Sentence(range) if range.end == (Position { line: 0, character: 10 })));
    }

    #[test]
    fn checked_frontier_requires_every_preceding_sentence_not_just_the_furthest_worker() {
        let doc = document("Check nat.\nCheck bool.\nCheck unit.");
        let rows = "[1] [Check--nat--.] (0 -> 10) (executed)\n[2] [Check--bool--.] (11 -> 22) (not executed)\n[3] \
                    [Check--unit--.] (23 -> 34) (executed in worker)";
        let navigation = Navigation::parse(&doc, &dump(rows)).unwrap();
        let first = super::Range::new(Position::new(0, 0), Position::new(0, 10));
        let second = super::Range::new(Position::new(1, 0), Position::new(1, 11));
        let third = super::Range::new(Position::new(2, 0), Position::new(2, 11));
        assert_eq!(navigation.checked(&doc, &[third]).unwrap(), None);
        assert_eq!(
            navigation.checked(&doc, &[third, first]).unwrap(),
            Some(Position::new(0, 10))
        );
        assert_eq!(
            navigation.checked(&doc, &[third, first, second]).unwrap(),
            Some(Position::new(2, 11))
        );
        let partial = super::Range::new(Position::new(0, 0), Position::new(0, 9));
        assert_eq!(navigation.checked(&doc, &[partial, second, third]).unwrap(), None);
    }

    #[test]
    fn checked_frontier_uses_server_sentence_boundaries_and_utf16_after_comments() {
        let doc = document("(* c *)\nCheck \"\u{1f9a4}\".");
        let navigation = Navigation::parse(
            &doc,
            &dump("(* comment *) (comment)\n[1] [Check--string--.] (8 -> 21) (executed)"),
        )
        .unwrap();
        let processed = super::Range::new(Position::new(1, 0), Position::new(1, 11));
        assert_eq!(
            navigation.checked(&doc, &[processed]).unwrap(),
            Some(Position::new(1, 11))
        );
    }

    #[test]
    fn checked_frontier_never_skips_a_parser_error_as_if_it_were_a_comment() {
        let doc = document("Check nat.\nFail.\nCheck bool.");
        let rows = "[1] [Check--nat--.] (0 -> 10) (executed)\n[parsing error] broken (11 -> 16) (error)\n[3] \
                    [Check--bool--.] (17 -> 28) (executed in worker)";
        let navigation = Navigation::parse(&doc, &dump(rows)).unwrap();
        let processed = super::Range::new(Position::new(0, 0), Position::new(2, 11));
        assert_eq!(
            navigation.checked(&doc, &[processed]).unwrap(),
            Some(Position::new(0, 10))
        );
    }

    #[test]
    fn unknown_or_inconsistent_server_dump_is_a_protocol_error() {
        let doc = document("Check nat.");
        assert!(matches!(
            Navigation::parse(&doc, "unrecognized state format"),
            Err(Error::UnknownHeader)
        ));
        assert!(matches!(
            Navigation::parse(&doc, &dump("[1] [Check--nat--.] (0 -> 99) (executed)")),
            Err(Error::InvalidSentenceRange {
                sentence: 1,
                start: 0,
                stop: 99,
                length: 10,
            })
        ));
        let mismatched = "Document using sentences_by_id map\n\n[1] [Check--nat--.] (0 -> 10) (executed)\n\nDocument \
                          using sentences_by_end map\n\n";
        assert!(matches!(
            Navigation::parse(&doc, mismatched),
            Err(Error::SentenceMapMismatch)
        ));
    }
}
