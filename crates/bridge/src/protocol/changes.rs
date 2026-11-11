use std::collections::BTreeMap;

use async_lsp::lsp_types::{TextDocumentContentChangeEvent, VersionedTextDocumentIdentifier};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{Extensible, Fields, Raw};

pub(crate) type Change = Extensible<TextDocumentContentChangeEvent>;
type DocumentVersion = Extensible<VersionedTextDocumentIdentifier>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChangeParams<T> {
    pub(crate) text_document: DocumentVersion,
    pub(crate) content_changes: T,

    #[serde(flatten)]
    pub(crate) extra: BTreeMap<String, Raw>,
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for ChangeParams<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let mut fields = Fields::deserialize(deserializer)?;
        Ok(Self {
            text_document: fields
                .take("textDocument")
                .map_err(serde::de::Error::custom)?
                .ok_or_else(|| serde::de::Error::missing_field("textDocument"))?,
            content_changes: fields
                .take("contentChanges")
                .map_err(serde::de::Error::custom)?
                .ok_or_else(|| serde::de::Error::missing_field("contentChanges"))?,
            extra: fields.rest(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Change, ChangeParams};
    use crate::{
        document::Document,
        protocol::{Fields, decode, raw},
    };

    #[test]
    fn normalized_replacements_preserve_editor_extensions() {
        let mut params: ChangeParams<Vec<Change>> = serde_json::from_str(
            r#"{
            "textDocument":{"uri":"file:///proof.v","version":2,"documentExtension":{"n":1}},
            "contentChanges":[{"text":"Check nat.","editExtension":[true,null]}],
            "batchExtension":"opaque"
        }"#,
        )
        .unwrap();
        let mut document = Document {
            uri: "file:///proof.v".into(),
            language: "rocq".into(),
            version: 1,
            text: "old".into(),
        };
        document
            .change(2, params.content_changes.iter_mut().map(|change| &mut change.value))
            .unwrap();
        let mut encoded: Fields = decode(&raw(&params).unwrap()).unwrap();
        assert_eq!(
            encoded.take::<String>("batchExtension").unwrap().as_deref(),
            Some("opaque")
        );
        let mut identifier: Fields = encoded.take("textDocument").unwrap().unwrap();
        assert_eq!(
            identifier
                .take::<super::Raw>("documentExtension")
                .unwrap()
                .unwrap()
                .get(),
            r#"{"n":1}"#
        );
        let edits: Vec<Fields> = encoded.take("contentChanges").unwrap().unwrap();
        let mut edit = edits.into_iter().next().unwrap();
        assert_eq!(
            edit.take::<super::Raw>("editExtension").unwrap().unwrap().get(),
            "[true,null]"
        );
        let range = edit.take::<async_lsp::lsp_types::Range>("range").unwrap().unwrap();
        assert_eq!(range.end.character, 3);
        assert_eq!(document.text, "Check nat.");
    }
}
