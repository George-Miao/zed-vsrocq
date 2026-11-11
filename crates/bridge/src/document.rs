use std::{num::TryFromIntError, path::PathBuf};

pub(crate) use async_lsp::lsp_types::Position;
use async_lsp::lsp_types::{Range, TextDocumentContentChangeEvent, TextDocumentItem};
use serde::Serialize;
use snafu::{OptionExt, ResultExt, Snafu, ensure};
use url::Url;

#[derive(Debug, Snafu)]
pub(crate) enum Error {
    #[snafu(display("didChange has a reversed range: {range:?}"))]
    ReversedRange { range: Range },

    #[snafu(display("server sentence offset {byte} is not a UTF-8 boundary"))]
    InvalidByteOffset { byte: usize },

    #[snafu(display("document has too many lines: {lines}"))]
    LineCountOverflow { lines: usize, source: TryFromIntError },

    #[snafu(display("document line has too many UTF-16 units: {units}"))]
    LineWidthOverflow { units: usize, source: TryFromIntError },

    #[snafu(display("LSP line {line} is outside the open document"))]
    LineOutsideDocument { line: u32 },

    #[snafu(display("LSP position {position:?} splits a UTF-16 surrogate pair"))]
    SplitSurrogate { position: Position },

    #[snafu(display("LSP position {position:?} is outside the open document line"))]
    CharacterOutsideLine { position: Position },
}

pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Clone, Debug)]
pub(crate) struct Document {
    pub(crate) uri: String,
    pub(crate) language: String,
    pub(crate) version: i64,
    pub(crate) text: String,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct Label {
    pub(crate) uri: String,
    pub(crate) name: String,
    pub(crate) version: i64,
}

impl Document {
    pub(crate) fn open(text_document: TextDocumentItem) -> Self {
        Self {
            uri: text_document.uri.into(),
            language: text_document.language_id,
            version: i64::from(text_document.version),
            text: text_document.text,
        }
    }

    pub(crate) fn change<'a>(
        &mut self,
        version: i64,
        changes: impl IntoIterator<Item = &'a mut TextDocumentContentChangeEvent>,
    ) -> Result<()> {
        // Apply the batch to a temporary value so a malformed edit cannot leave
        // half a batch.
        let mut text = self.text.clone();
        for change in changes {
            if let Some(range) = change.range {
                let start = offset(&text, range.start)?;
                let end = offset(&text, range.end)?;
                ensure!(start <= end, ReversedRangeSnafu { range });
                text.replace_range(start..end, &change.text);
            } else {
                // VsRocq requires a range even for legal full-document
                // replacements.
                let end = end_position(&text)?;
                text.clear();
                text.push_str(&change.text);
                change.range = Some(Range {
                    start: Position::default(),
                    end,
                });
            }
        }
        self.text = text;
        self.version = version;
        Ok(())
    }

    pub(crate) fn validate_position(&self, position: Position) -> Result<Position> {
        offset(&self.text, position)?;
        Ok(position)
    }

    pub(crate) fn byte_offset(&self, position: Position) -> Result<usize> {
        offset(&self.text, position)
    }

    pub(crate) fn position_at(&self, byte: usize) -> Result<Position> {
        let prefix = self.text.get(..byte).context(InvalidByteOffsetSnafu { byte })?;
        end_position(prefix)
    }

    pub(crate) fn label(&self) -> Label {
        let name = local_path(&self.uri)
            .and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
            .unwrap_or_else(|| self.uri.clone());
        Label {
            uri: self.uri.clone(),
            name,
            version: self.version,
        }
    }
}

pub(crate) fn local_path(uri: &str) -> Option<PathBuf> {
    Url::parse(uri).ok()?.to_file_path().ok()
}

fn end_position(text: &str) -> Result<Position> {
    let lines = text.bytes().filter(|byte| *byte == b'\n').count();
    let line = u32::try_from(lines).context(LineCountOverflowSnafu { lines })?;
    let last_line = text.rsplit_once('\n').map_or(text, |(_, line)| line);
    let units = last_line.encode_utf16().count();
    let character = u32::try_from(units).context(LineWidthOverflowSnafu { units })?;
    Ok(Position { line, character })
}

fn line_text(text: &str, line: usize) -> Option<&str> {
    let line = text.split('\n').nth(line)?;
    Some(line.strip_suffix('\r').unwrap_or(line))
}

fn offset(text: &str, position: Position) -> Result<usize> {
    let mut start = 0;
    for _ in 0..position.line {
        let length = text[start..]
            .find('\n')
            .context(LineOutsideDocumentSnafu { line: position.line })?;
        start += length + 1;
    }
    let line = line_text(&text[start..], 0).context(LineOutsideDocumentSnafu { line: position.line })?;
    let mut units = 0;
    for (byte, character) in line.char_indices() {
        if units == position.character {
            return Ok(start + byte);
        }
        units += character.len_utf16() as u32;
        ensure!(units <= position.character, SplitSurrogateSnafu { position });
    }
    ensure!(units == position.character, CharacterOutsideLineSnafu { position });
    Ok(start + line.len())
}

#[cfg(test)]
mod tests {
    use async_lsp::lsp_types::{Position, Range, TextDocumentContentChangeEvent, TextDocumentItem};

    use super::{Document, Error};

    fn document(text: &str) -> Document {
        Document::open(TextDocumentItem {
            uri: "file:///proof.v".parse().unwrap(),
            language_id: "rocq".into(),
            version: 1,
            text: text.into(),
        })
    }

    fn range(start: (u32, u32), end: (u32, u32)) -> Range {
        Range {
            start: Position::new(start.0, start.1),
            end: Position::new(end.0, end.1),
        }
    }

    fn change(range: Option<Range>, text: &str) -> TextDocumentContentChangeEvent {
        TextDocumentContentChangeEvent {
            range,
            range_length: None,
            text: text.into(),
        }
    }

    #[test]
    fn opens_typed_documents_without_narrowing_versions() {
        let doc = Document::open(TextDocumentItem {
            uri: "file:///proof.v".parse().unwrap(),
            language_id: "coq".into(),
            version: i32::MIN,
            text: "Check nat.".into(),
        });
        assert_eq!(doc.uri, "file:///proof.v");
        assert_eq!(doc.language, "coq");
        assert_eq!(doc.version, i64::from(i32::MIN));
        assert_eq!(doc.text, "Check nat.");
    }

    #[test]
    fn applies_incremental_edits_against_each_previous_change() {
        let mut doc = document("a\u{1f9a4}b\nCheck nat.");
        doc.change(2, &mut [
            change(Some(range((0, 1), (0, 3))), "x"),
            change(Some(range((0, 2), (1, 0))), ".\n"),
        ])
        .unwrap();
        assert_eq!(doc.text, "ax.\nCheck nat.");
        assert_eq!(doc.version, 2);
    }

    #[test]
    fn full_replacements_are_forwarded_as_sequential_utf16_ranges() {
        let mut doc = document("old");
        let mut changes = [
            change(Some(range((0, 0), (0, 3))), "a\r\nb\u{1f9a4}"),
            change(None, "Lemma demo : False.\nProof.\n  exact I.\nQed.\n"),
            change(None, "Check nat."),
        ];
        let version = i64::from(i32::MAX) + 1;
        doc.change(version, &mut changes).unwrap();
        assert_eq!(changes[1].range, Some(range((0, 0), (1, 3))));
        assert_eq!(changes[2].range, Some(range((0, 0), (4, 0))));
        assert_eq!(doc.text, "Check nat.");
        assert_eq!(doc.version, version);
    }

    #[test]
    fn rejects_split_surrogates_without_committing_a_partial_batch() {
        let mut doc = document("\u{1f9a4}");
        assert!(matches!(
            doc.change(2, &mut [change(Some(range((0, 1), (0, 2))), "x")]),
            Err(Error::SplitSurrogate {
                position: Position { line: 0, character: 1 },
            })
        ));
        assert_eq!(doc.version, 1);
        assert_eq!(doc.text, "\u{1f9a4}");
    }

    #[test]
    fn rejects_a_later_invalid_range_without_committing_earlier_changes() {
        let mut doc = document("old");
        assert!(matches!(
            doc.change(2, &mut [
                change(None, "a\u{1f9a4}"),
                change(Some(range((0, 1), (0, 2))), "x"),
            ]),
            Err(Error::SplitSurrogate {
                position: Position { line: 0, character: 2 },
            })
        ));
        assert_eq!(doc.version, 1);
        assert_eq!(doc.text, "old");
    }
}
