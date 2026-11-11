use std::collections::BTreeMap;

use async_lsp::lsp_types::{InlayHint, InlayHintLabel, Position, Range};
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeSeq};
use serde_json::value::RawValue;

use super::{Failure, Fields, Raw, Result, decode, raw};

const CHECKED: &str = "Checked to here";
const CHECKING: &str = "Checking...";

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum Provider {
    Simple(bool),
    Options(Options),
}

impl Provider {
    pub(crate) fn resolve_support(&self) -> bool {
        matches!(
            self,
            Self::Options(Options {
                resolve_provider: Some(true),
                ..
            })
        )
    }
}

impl<'de> Deserialize<'de> for Provider {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let value = Raw::deserialize(deserializer)?;
        if let Ok(enabled) = decode::<bool>(&value) {
            return Ok(Self::Simple(enabled));
        }
        decode(&value).map(Self::Options).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", try_from = "Fields")]
pub(crate) struct Options {
    #[serde(skip_serializing_if = "Option::is_none")]
    resolve_provider: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    work_done_progress: Option<bool>,

    #[serde(flatten)]
    extra: BTreeMap<String, Raw>,
}

impl TryFrom<Fields> for Options {
    type Error = Failure;

    fn try_from(mut fields: Fields) -> Result<Self> {
        Ok(Self {
            resolve_provider: fields.take::<Option<bool>>("resolveProvider")?.flatten(),
            work_done_progress: fields.take::<Option<bool>>("workDoneProgress")?.flatten(),
            extra: fields.rest(),
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct Identity<'a> {
    pub(crate) uri: &'a str,
    pub(crate) version: i64,
    pub(crate) epoch: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub(crate) uri: String,
    pub(crate) version: i64,
    pub(crate) epoch: u64,
    pub(crate) position: Position,
    pub(crate) label: String,
}

impl Resolved {
    pub(crate) fn matches(&self, checked: Option<Position>, checking: Option<Position>) -> bool {
        match self.label.as_str() {
            CHECKED => checked == Some(self.position) && checked != checking,
            CHECKING => checking == Some(self.position),
            _ => false,
        }
    }
}

#[derive(Deserialize)]
struct ResolveParams {
    data: Option<Raw>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    uri: String,
    version: i64,
    epoch: u64,
}

#[derive(Deserialize)]
struct ResolvedHint {
    position: Position,
    label: String,
}

pub(crate) fn resolve(params: &RawValue) -> Result<Option<Resolved>> {
    let input: ResolveParams = decode(params)?;
    let Some(data) = input.data else {
        return Ok(None);
    };
    let Ok(mut fields) = decode::<Fields>(&data) else {
        return Ok(None);
    };
    let Some(identity) = fields.take::<Raw>("zed-vsrocq.progress")? else {
        return Ok(None);
    };
    if !fields.rest().is_empty() {
        return Ok(None);
    }
    let ownership: Ownership = decode(&identity)?;
    let hint: ResolvedHint = decode(params)?;
    Ok(Some(Resolved {
        uri: ownership.uri,
        version: ownership.version,
        epoch: ownership.epoch,
        position: hint.position,
        label: hint.label,
    }))
}

pub(crate) fn advertise(provider: &mut Option<Provider>) -> bool {
    let native = provider
        .as_ref()
        .is_some_and(|provider| !matches!(provider, Provider::Simple(false)));
    if !native {
        *provider = Some(Provider::Options(Options::default()));
    }
    native
}

pub(crate) fn hints(checked: Option<Position>, checking: Option<Position>, range: Range) -> Vec<InlayHint> {
    let mut hints = Vec::new();
    if let Some(position) = checked.filter(|position| Some(*position) != checking && contains(range, *position)) {
        hints.push(hint(position, CHECKED));
    }
    if let Some(position) = checking.filter(|position| contains(range, *position)) {
        hints.push(hint(position, CHECKING));
    }
    hints
}

fn contains(range: Range, position: Position) -> bool {
    // Hints mark insertion positions, including the requested end boundary.
    range.start <= position && position <= range.end
}

fn hint(position: Position, label: &str) -> InlayHint {
    InlayHint {
        position,
        label: InlayHintLabel::String(label.into()),
        kind: None,
        text_edits: None,
        tooltip: None,
        padding_left: Some(true),
        padding_right: None,
        data: None,
    }
}

pub(crate) fn merge(native: Option<&RawValue>, local: &[InlayHint], identity: Identity<'_>) -> Result<Raw> {
    if local.is_empty() {
        return native.map_or_else(|| raw(local), |native| Ok(native.to_owned()));
    }
    let native: Option<Vec<Raw>> = native.map(decode).transpose()?.flatten();
    let native = native.unwrap_or_default();
    raw(&Merged {
        native: &native,
        local,
        identity,
    })
}

struct Merged<'a> {
    native: &'a [Raw],
    local: &'a [InlayHint],
    identity: Identity<'a>,
}

impl Serialize for Merged<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.native.len() + self.local.len()))?;
        for hint in self.native {
            seq.serialize_element(hint)?;
        }
        for hint in self.local {
            seq.serialize_element(&Local {
                hint,
                data: HintData {
                    identity: self.identity,
                },
            })?;
        }
        seq.end()
    }
}

#[derive(Serialize)]
struct Local<'a> {
    #[serde(flatten)]
    hint: &'a InlayHint,
    data: HintData<'a>,
}

#[derive(Serialize)]
struct HintData<'a> {
    #[serde(rename = "zed-vsrocq.progress")]
    identity: Identity<'a>,
}

#[cfg(test)]
mod tests {
    use async_lsp::lsp_types::{InlayHintLabel, Position, Range};
    use serde_json::value::RawValue;

    use super::{Identity, Provider, Resolved, advertise, hints, merge, resolve};
    use crate::protocol::{Fields, Raw, decode, raw};

    fn range(start: (u32, u32), end: (u32, u32)) -> Range {
        Range::new(Position::new(start.0, start.1), Position::new(end.0, end.1))
    }

    fn identity() -> Identity<'static> {
        Identity {
            uri: "file:///proof.v",
            version: 7,
            epoch: 42,
        }
    }

    #[test]
    fn advertises_options_without_claiming_native_support() {
        for native in [None, Some(Provider::Simple(false))] {
            let mut provider = native;
            assert!(!advertise(&mut provider));
            assert_eq!(raw(&provider).unwrap().get(), "{}");
            assert!(!provider.as_ref().unwrap().resolve_support());
        }
        let mut provider = Some(Provider::Simple(true));
        assert!(advertise(&mut provider));
        assert_eq!(raw(&provider).unwrap().get(), "true");
        assert!(!provider.as_ref().unwrap().resolve_support());
    }

    #[test]
    fn native_provider_keeps_resolution_and_registration_extensions() {
        let mut provider: Option<Provider> = serde_json::from_str(
            r#"{
                "resolveProvider":true,
                "workDoneProgress":false,
                "id":"native-hints",
                "documentSelector":[{"language":"rocq","futureFilter":1.00e+2}],
                "futureOptions":{"opaque":[null,1.00e+2]}
            }"#,
        )
        .unwrap();
        assert!(advertise(&mut provider));
        assert!(provider.as_ref().unwrap().resolve_support());
        let mut encoded: Fields = decode(&raw(&provider).unwrap()).unwrap();
        assert_eq!(encoded.take::<bool>("resolveProvider").unwrap(), Some(true));
        assert_eq!(encoded.take::<bool>("workDoneProgress").unwrap(), Some(false));
        assert_eq!(encoded.take::<String>("id").unwrap().as_deref(), Some("native-hints"));
        assert_eq!(
            encoded.take::<Raw>("documentSelector").unwrap().unwrap().get(),
            r#"[{"language":"rocq","futureFilter":1.00e+2}]"#
        );
        assert_eq!(
            encoded.take::<Raw>("futureOptions").unwrap().unwrap().get(),
            r#"{"opaque":[null,1.00e+2]}"#
        );
    }

    #[test]
    fn hint_labels_are_truthful_and_do_not_claim_a_type_or_parameter_kind() {
        let local = hints(
            Some(Position::new(1, 7)),
            Some(Position::new(2, 3)),
            range((0, 0), (3, 0)),
        );
        assert_eq!(local.len(), 2);
        assert!(matches!(&local[0].label, InlayHintLabel::String(label) if label == "Checked to here"));
        assert!(matches!(&local[1].label, InlayHintLabel::String(label) if label == "Checking..."));
        let encoded: Vec<Raw> = decode(&raw(&local).unwrap()).unwrap();
        for hint in encoded {
            let mut hint: Fields = decode(&hint).unwrap();
            assert!(hint.take::<Raw>("kind").unwrap().is_none());
            assert!(hint.take::<Raw>("textEdits").unwrap().is_none());
            assert!(hint.take::<Raw>("data").unwrap().is_none());
        }
    }

    #[test]
    fn filters_hint_insertion_positions_to_the_requested_range() {
        let requested = range((1, 4), (2, 3));
        let boundary = hints(Some(requested.start), Some(requested.end), requested);
        assert_eq!(boundary.len(), 2);
        assert!(hints(Some(Position::new(1, 3)), Some(Position::new(2, 4)), requested).is_empty());
        assert!(hints(Some(Position::new(0, 9)), Some(Position::new(3, 0)), requested).is_empty());
        assert!(hints(None, None, requested).is_empty());
        assert!(hints(Some(requested.start), None, range((2, 0), (1, 0))).is_empty());
    }

    #[test]
    fn shows_only_checking_when_both_markers_share_an_insertion_position() {
        let position = Position::new(1, 4);
        let local = hints(Some(position), Some(position), Range::new(position, position));
        assert_eq!(local.len(), 1);
        assert!(matches!(&local[0].label, InlayHintLabel::String(label) if label == "Checking..."));
    }

    #[test]
    fn merge_preserves_native_hint_objects_and_nested_raw_metadata() {
        let native = r#"{"position":{"line":0,"character":1},"label":[{"value":"native","futurePart":[null,1.00e+2]}],"data":{"futureData":1.00e+2},"futureHint":true}"#;
        let native_result = RawValue::from_string(format!("[{native}]")).unwrap();
        let local = hints(Some(Position::new(1, 4)), None, range((0, 0), (2, 0)));
        let merged = merge(Some(&native_result), &local, identity()).unwrap();
        let merged: Vec<Raw> = decode(&merged).unwrap();
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].get(), native);
        let mut local: Fields = decode(&merged[1]).unwrap();
        assert_eq!(local.take::<Position>("position").unwrap(), Some(Position::new(1, 4)));
    }

    #[test]
    fn merge_handles_absent_null_and_empty_native_results() {
        let local = hints(Some(Position::new(1, 4)), None, range((0, 0), (2, 0)));
        for native in [
            None,
            Some(RawValue::from_string("null".into()).unwrap()),
            Some(RawValue::from_string("[]".into()).unwrap()),
        ] {
            let merged = merge(native.as_deref(), &local, identity()).unwrap();
            assert_eq!(decode::<Vec<Raw>>(&merged).unwrap().len(), 1);
        }
        assert_eq!(merge(None, &[], identity()).unwrap().get(), "[]");
        let native = RawValue::from_string("null".into()).unwrap();
        assert_eq!(merge(Some(&native), &[], identity()).unwrap().get(), "null");
    }

    #[test]
    fn merge_rejects_invalid_native_arrays_instead_of_losing_them() {
        let local = hints(Some(Position::new(1, 4)), None, range((0, 0), (2, 0)));
        let native = RawValue::from_string(r#"{"unexpected":true}"#.into()).unwrap();
        assert!(merge(Some(&native), &local, identity()).is_err());
    }

    #[test]
    fn synthetic_hints_identify_the_document_revision_for_local_resolution() {
        let local = hints(Some(Position::new(1, 4)), None, range((0, 0), (2, 0)));
        let merged = merge(None, &local, identity()).unwrap();
        let merged: Vec<Raw> = decode(&merged).unwrap();
        assert_eq!(
            resolve(&merged[0]).unwrap(),
            Some(Resolved {
                uri: "file:///proof.v".into(),
                version: 7,
                epoch: 42,
                position: Position::new(1, 4),
                label: "Checked to here".into(),
            })
        );
        let mut hint: Fields = decode(&merged[0]).unwrap();
        assert!(hint.take::<Raw>("kind").unwrap().is_none());
        assert!(hint.take::<Raw>("textEdits").unwrap().is_none());
    }

    #[test]
    fn local_resolution_rejects_moved_cleared_and_superseded_markers() {
        let position = Position::new(1, 4);
        let mut resolved = Resolved {
            uri: "file:///proof.v".into(),
            version: 7,
            epoch: 42,
            position,
            label: "Checked to here".into(),
        };
        assert!(resolved.matches(Some(position), None));
        assert!(!resolved.matches(None, None));
        assert!(!resolved.matches(Some(Position::new(1, 3)), None));
        assert!(!resolved.matches(Some(position), Some(position)));
        resolved.label = "Checking...".into();
        assert!(resolved.matches(None, Some(position)));
        assert!(resolved.matches(Some(position), Some(position)));
        assert!(!resolved.matches(Some(position), None));
        assert!(!resolved.matches(None, Some(Position::new(1, 3))));
        resolved.label = "native".into();
        assert!(!resolved.matches(Some(position), Some(position)));
    }

    #[test]
    fn resolution_does_not_claim_native_hints_with_arbitrary_data() {
        for data in [
            "null",
            "true",
            "17",
            r#""native""#,
            r#"["native",true]"#,
            r#"{"futureData":1.00e+2}"#,
            r#"{"zed-vsrocq.progress":{"uri":"file:///proof.v","version":7,"epoch":42},"futureData":true}"#,
        ] {
            let hint = RawValue::from_string(format!(
                r#"{{"position":{{"line":1,"character":4}},"label":"Checked to here","data":{data}}}"#
            ))
            .unwrap();
            assert_eq!(resolve(&hint).unwrap(), None, "{data}");
        }
        let hint = raw(&hints(Some(Position::new(1, 4)), None, range((0, 0), (2, 0)))[0]).unwrap();
        assert_eq!(resolve(&hint).unwrap(), None);
    }

    #[test]
    fn malformed_bridge_ownership_is_not_forwarded_to_native_resolution() {
        let hint = RawValue::from_string(
            r#"{"data":{"zed-vsrocq.progress":{"uri":"file:///proof.v","version":"invalid","epoch":42}}}"#.into(),
        )
        .unwrap();
        assert!(resolve(&hint).is_err());
    }
}
