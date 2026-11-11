use std::fmt;

use async_lsp::lsp_types::{DiagnosticSeverity, Range};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor, value::MapAccessDeserializer},
    ser::SerializeSeq,
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct ProofView {
    pub(crate) range: Range,
    pub(crate) proof: Option<Goals<RichGoal>>,
    pub(crate) messages: Vec<(DiagnosticSeverity, Pp)>,
    pub(crate) pp_proof: Option<Goals<PlainGoal>>,
    pub(crate) pp_messages: Vec<(DiagnosticSeverity, String)>,
}

impl ProofView {
    pub(crate) fn failed(&self) -> bool {
        self.messages
            .iter()
            .any(|(severity, _)| *severity == DiagnosticSeverity::ERROR)
            || self
                .pp_messages
                .iter()
                .any(|(severity, _)| *severity == DiagnosticSeverity::ERROR)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Goals<G> {
    pub(crate) goals: Vec<G>,
    pub(crate) shelved_goals: Vec<G>,
    pub(crate) given_up_goals: Vec<G>,
    pub(crate) unfocused_goals: Vec<G>,
}

impl<G> Default for Goals<G> {
    fn default() -> Self {
        Self {
            goals: Vec::new(),
            shelved_goals: Vec::new(),
            given_up_goals: Vec::new(),
            unfocused_goals: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct RichGoal {
    pub(crate) id: i64,
    pub(crate) name: Option<String>,
    pub(crate) hypotheses: Vec<Pp>,
    pub(crate) goal: Pp,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct PlainGoal {
    pub(crate) id: i64,
    pub(crate) name: Option<String>,
    pub(crate) hypotheses: Vec<PlainHypothesis>,
    pub(crate) goal: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct PlainHypothesis {
    pub(crate) ids: Vec<String>,
    pub(crate) body: Option<String>,
    pub(crate) _type: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Pp {
    #[default]
    Empty,

    String(String),
    Glue(Vec<Pp>),
    Box(Block, Box<Pp>),
    Tag(String, Box<Pp>),
    PrintBreak(i64, i64),
    ForceNewline,
    Comment(Vec<String>),
}

impl Serialize for Pp {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let len = match self {
            Self::Empty | Self::ForceNewline => 1,
            Self::String(_) | Self::Glue(_) | Self::Comment(_) => 2,
            Self::Box(..) | Self::Tag(..) | Self::PrintBreak(..) => 3,
        };
        let mut seq = serializer.serialize_seq(Some(len))?;
        match self {
            Self::Empty => seq.serialize_element("Ppcmd_empty")?,
            Self::String(text) => {
                seq.serialize_element("Ppcmd_string")?;
                seq.serialize_element(text)?;
            }
            Self::Glue(parts) => {
                seq.serialize_element("Ppcmd_glue")?;
                seq.serialize_element(parts)?;
            }
            Self::Box(block, pp) => {
                seq.serialize_element("Ppcmd_box")?;
                seq.serialize_element(block)?;
                seq.serialize_element(pp)?;
            }
            Self::Tag(tag, pp) => {
                seq.serialize_element("Ppcmd_tag")?;
                seq.serialize_element(tag)?;
                seq.serialize_element(pp)?;
            }
            Self::PrintBreak(spaces, indent) => {
                seq.serialize_element("Ppcmd_print_break")?;
                seq.serialize_element(spaces)?;
                seq.serialize_element(indent)?;
            }
            Self::ForceNewline => seq.serialize_element("Ppcmd_force_newline")?,
            Self::Comment(lines) => {
                seq.serialize_element("Ppcmd_comment")?;
                seq.serialize_element(lines)?;
            }
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for Pp {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(PpVisitor)
    }
}

#[derive(Deserialize)]
#[serde(field_identifier)]
enum PpTag {
    #[serde(rename = "Ppcmd_empty")]
    Empty,

    #[serde(rename = "Ppcmd_string")]
    String,

    #[serde(rename = "Ppcmd_glue")]
    Glue,

    #[serde(rename = "Ppcmd_box")]
    Box,

    #[serde(rename = "Ppcmd_tag")]
    Tag,

    #[serde(rename = "Ppcmd_print_break")]
    PrintBreak,

    #[serde(rename = "Ppcmd_force_newline")]
    ForceNewline,

    #[serde(rename = "Ppcmd_comment")]
    Comment,
}

struct PpVisitor;

impl<'de> Visitor<'de> for PpVisitor {
    type Value = Pp;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a tagged VsRocq pretty-print command array")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Pp, A::Error> {
        let pp = match next(&mut seq, 0, &self)? {
            PpTag::Empty => Pp::Empty,
            PpTag::String => Pp::String(next(&mut seq, 1, &self)?),
            PpTag::Glue => Pp::Glue(next(&mut seq, 1, &self)?),
            PpTag::Box => Pp::Box(next(&mut seq, 1, &self)?, Box::new(next(&mut seq, 2, &self)?)),
            PpTag::Tag => Pp::Tag(next(&mut seq, 1, &self)?, Box::new(next(&mut seq, 2, &self)?)),
            PpTag::PrintBreak => Pp::PrintBreak(next(&mut seq, 1, &self)?, next(&mut seq, 2, &self)?),
            PpTag::ForceNewline => Pp::ForceNewline,
            PpTag::Comment => Pp::Comment(next(&mut seq, 1, &self)?),
        };
        end(&mut seq)?;
        Ok(pp)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Block {
    Hbox,
    Vbox(i64),
    Hvbox(i64),
    Hovbox(i64),
}

impl Serialize for Block {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(if matches!(self, Self::Hbox) { 1 } else { 2 }))?;
        match self {
            Self::Hbox => seq.serialize_element("Pp_hbox")?,
            Self::Vbox(indent) => {
                seq.serialize_element("Pp_vbox")?;
                seq.serialize_element(indent)?;
            }
            Self::Hvbox(indent) => {
                seq.serialize_element("Pp_hvbox")?;
                seq.serialize_element(indent)?;
            }
            Self::Hovbox(indent) => {
                seq.serialize_element("Pp_hovbox")?;
                seq.serialize_element(indent)?;
            }
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for Block {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(BlockVisitor)
    }
}

#[derive(Deserialize)]
#[serde(field_identifier)]
enum BlockTag {
    #[serde(rename = "Pp_hbox")]
    Hbox,

    #[serde(rename = "Pp_vbox")]
    Vbox,

    #[serde(rename = "Pp_hvbox")]
    Hvbox,

    #[serde(rename = "Pp_hovbox")]
    Hovbox,
}

struct BlockVisitor;

impl<'de> Visitor<'de> for BlockVisitor {
    type Value = Block;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a tagged VsRocq pretty-print block array")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Block, A::Error> {
        let block = match next(&mut seq, 0, &self)? {
            BlockTag::Hbox => Block::Hbox,
            BlockTag::Vbox => Block::Vbox(next(&mut seq, 1, &self)?),
            BlockTag::Hvbox => Block::Hvbox(next(&mut seq, 1, &self)?),
            BlockTag::Hovbox => Block::Hovbox(next(&mut seq, 1, &self)?),
        };
        end(&mut seq)?;
        Ok(block)
    }
}

fn next<'de, A: SeqAccess<'de>, T: Deserialize<'de>>(
    seq: &mut A,
    index: usize,
    expected: &dyn de::Expected,
) -> Result<T, A::Error> {
    seq.next_element()?
        .ok_or_else(|| de::Error::invalid_length(index, expected))
}

fn end<'de, A: SeqAccess<'de>>(seq: &mut A) -> Result<(), A::Error> {
    if seq.next_element::<IgnoredAny>()?.is_some() {
        return Err(de::Error::custom("unexpected trailing pretty-print array element"));
    }
    Ok(())
}

// This is a private-session summary, not the forwarded LSP diagnostic payload.
// Incomplete ranges are intentional: older fixtures and servers may omit end or
// character.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Diagnostics {
    pub(crate) uri: String,
    pub(crate) version: Option<i64>,

    #[serde(default)]
    diagnostics: Vec<Diagnostic>,
}

impl Diagnostics {
    pub(crate) fn errors(&self) -> Vec<String> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Some(DiagnosticSeverity::ERROR))
            .map(|diagnostic| {
                let message = diagnostic.message.as_deref().unwrap_or("Rocq reported an error");
                match diagnostic
                    .range
                    .as_ref()
                    .and_then(|range| range.start.as_ref())
                    .and_then(|start| start.line)
                {
                    Some(line) => format!("Line {}: {message}", line.saturating_add(1)),
                    None => message.to_owned(),
                }
            })
            .collect()
    }
}

#[derive(Debug, Deserialize)]
struct Diagnostic {
    severity: Option<DiagnosticSeverity>,
    range: Option<DiagnosticRange>,

    #[serde(default, deserialize_with = "message")]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DiagnosticRange {
    start: Option<DiagnosticStart>,
}

#[derive(Debug, Deserialize)]
struct DiagnosticStart {
    #[serde(default, deserialize_with = "line")]
    line: Option<u64>,
}

fn message<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    deserializer.deserialize_any(MessageVisitor { object: true })
}

fn text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    deserializer.deserialize_any(MessageVisitor { object: false })
}

struct MessageVisitor {
    object: bool,
}

impl<'de> Visitor<'de> for MessageVisitor {
    type Value = Option<String>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a diagnostic message string or object with a string value")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Some(value.to_owned()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(Some(value))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        if !self.object {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            return Ok(None);
        }

        #[derive(Deserialize)]
        struct Message {
            #[serde(default, deserialize_with = "text")]
            value: Option<String>,
        }

        Message::deserialize(MapAccessDeserializer::new(map)).map(|message| message.value)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(None)
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }
}

fn line<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    deserializer.deserialize_any(LineVisitor)
}

struct LineVisitor;

impl<'de> Visitor<'de> for LineVisitor {
    type Value = Option<u64>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an optional unsigned diagnostic start line")
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Some(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(u64::try_from(value).ok())
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(None)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_print_commands_keep_their_tagged_array_encoding() {
        let cases = [
            (Pp::Empty, r#"["Ppcmd_empty"]"#),
            (Pp::String("x".into()), r#"["Ppcmd_string","x"]"#),
            (
                Pp::Glue(vec![Pp::String("x".into()), Pp::ForceNewline]),
                r#"["Ppcmd_glue",[["Ppcmd_string","x"],["Ppcmd_force_newline"]]]"#,
            ),
            (
                Pp::Box(Block::Hovbox(-2), Box::new(Pp::Empty)),
                r#"["Ppcmd_box",["Pp_hovbox",-2],["Ppcmd_empty"]]"#,
            ),
            (
                Pp::Tag("constr.variable".into(), Box::new(Pp::String("x".into()))),
                r#"["Ppcmd_tag","constr.variable",["Ppcmd_string","x"]]"#,
            ),
            (Pp::PrintBreak(1, -3), r#"["Ppcmd_print_break",1,-3]"#),
            (Pp::ForceNewline, r#"["Ppcmd_force_newline"]"#),
            (
                Pp::Comment(vec!["first".into(), "second".into()]),
                r#"["Ppcmd_comment",["first","second"]]"#,
            ),
        ];
        for (pp, wire) in cases {
            assert_eq!(serde_json::to_string(&pp).unwrap(), wire);
            assert_eq!(serde_json::from_str::<Pp>(wire).unwrap(), pp);
        }
    }

    #[test]
    fn pretty_print_blocks_keep_all_four_constructors() {
        let cases = [
            (Block::Hbox, r#"["Pp_hbox"]"#),
            (Block::Vbox(-1), r#"["Pp_vbox",-1]"#),
            (Block::Hvbox(2), r#"["Pp_hvbox",2]"#),
            (Block::Hovbox(3), r#"["Pp_hovbox",3]"#),
        ];
        for (block, wire) in cases {
            assert_eq!(serde_json::to_string(&block).unwrap(), wire);
            assert_eq!(serde_json::from_str::<Block>(wire).unwrap(), block);
        }
    }

    #[test]
    fn pretty_print_arrays_require_known_tags_and_exact_arity() {
        for wire in [
            r#"[]"#,
            r#"["Ppcmd_unknown"]"#,
            r#"[0]"#,
            r#"[{"Ppcmd_empty":null}]"#,
            r#"["Ppcmd_empty",0]"#,
            r#"["Ppcmd_string"]"#,
            r#"["Ppcmd_string",2]"#,
            r#"["Ppcmd_box",["Pp_hbox"]]"#,
            r#"["Ppcmd_tag","tag",["Ppcmd_empty"],0]"#,
            r#"["Ppcmd_print_break",1]"#,
            r#"["Ppcmd_comment","not a list"]"#,
            r#"{"Ppcmd_empty":null}"#,
        ] {
            assert!(serde_json::from_str::<Pp>(wire).is_err(), "{wire}");
        }
        for wire in [
            r#"[]"#,
            r#"["Pp_unknown"]"#,
            r#"[0]"#,
            r#"[{"Pp_hbox":null}]"#,
            r#"["Pp_hbox",0]"#,
            r#"["Pp_vbox"]"#,
        ] {
            assert!(serde_json::from_str::<Block>(wire).is_err(), "{wire}");
        }
    }

    #[test]
    fn top_view_serializes_both_proof_branches_as_null() {
        let wire = concat!(
            r#"{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"#,
            r#""proof":null,"messages":[],"pp_proof":null,"pp_messages":[]}"#,
        );
        assert_eq!(serde_json::to_string(&ProofView::default()).unwrap(), wire);
        assert_eq!(serde_json::from_str::<ProofView>(wire).unwrap(), ProofView::default());
    }

    #[test]
    fn rich_goals_keep_pretty_print_hypotheses_and_large_signed_ids() {
        let view = ProofView {
            proof: Some(Goals {
                goals: vec![RichGoal {
                    id: 4_294_967_296,
                    name: None,
                    hypotheses: vec![Pp::String("H : True".into())],
                    goal: Pp::Tag("constr.variable".into(), Box::new(Pp::String("H".into()))),
                }],
                ..Goals::default()
            }),
            messages: vec![(DiagnosticSeverity::WARNING, Pp::String("warning".into()))],
            ..ProofView::default()
        };
        let wire = concat!(
            r#"{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"#,
            r#""proof":{"goals":[{"id":4294967296,"name":null,"hypotheses":[["Ppcmd_string","H : True"]],"#,
            r#""goal":["Ppcmd_tag","constr.variable",["Ppcmd_string","H"]]}],"#,
            r#""shelvedGoals":[],"givenUpGoals":[],"unfocusedGoals":[]},"#,
            r#""messages":[[2,["Ppcmd_string","warning"]]],"pp_proof":null,"pp_messages":[]}"#,
        );
        assert_eq!(serde_json::to_string(&view).unwrap(), wire);
        assert_eq!(serde_json::from_str::<ProofView>(wire).unwrap(), view);
        assert!(!view.failed());
    }

    #[test]
    fn plain_goals_keep_declaration_objects_and_null_optional_fields() {
        let view = ProofView {
            pp_proof: Some(Goals {
                goals: vec![PlainGoal {
                    id: -4_294_967_296,
                    name: None,
                    hypotheses: vec![
                        PlainHypothesis {
                            ids: vec!["x".into(), "y".into()],
                            body: None,
                            _type: "nat".into(),
                        },
                        PlainHypothesis {
                            ids: vec!["z".into()],
                            body: Some("x".into()),
                            _type: "nat".into(),
                        },
                    ],
                    goal: "x = y".into(),
                }],
                ..Goals::default()
            }),
            pp_messages: vec![(DiagnosticSeverity::INFORMATION, "plain feedback".into())],
            ..ProofView::default()
        };
        let wire = concat!(
            r#"{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"#,
            r#""proof":null,"messages":[],"pp_proof":{"goals":[{"id":-4294967296,"name":null,"hypotheses":["#,
            r#"{"ids":["x","y"],"body":null,"_type":"nat"},{"ids":["z"],"body":"x","_type":"nat"}],"#,
            r#""goal":"x = y"}],"shelvedGoals":[],"givenUpGoals":[],"unfocusedGoals":[]},"#,
            r#""pp_messages":[[3,"plain feedback"]]}"#,
        );
        assert_eq!(serde_json::to_string(&view).unwrap(), wire);
        assert_eq!(serde_json::from_str::<ProofView>(wire).unwrap(), view);
        assert!(!view.failed());
    }

    #[test]
    fn proof_failure_comes_from_error_severity_in_either_message_list() {
        let mut view = ProofView {
            proof: Some(Goals {
                given_up_goals: vec![RichGoal::default()],
                ..Goals::default()
            }),
            messages: vec![(DiagnosticSeverity::WARNING, Pp::Empty)],
            pp_messages: vec![(DiagnosticSeverity::INFORMATION, "info".into())],
            ..ProofView::default()
        };
        assert!(!view.failed());
        view.messages
            .push((DiagnosticSeverity::ERROR, Pp::String("rich error".into())));
        assert!(view.failed());
        view.messages.pop();
        view.pp_messages.push((DiagnosticSeverity::ERROR, "plain error".into()));
        assert!(view.failed());
    }

    #[test]
    fn diagnostics_project_errors_without_requiring_complete_lsp_ranges() {
        let diagnostics: Diagnostics = serde_json::from_str(
            r#"{
                "uri":"file:///proof.v",
                "version":4294967296,
                "diagnostics":[
                    {"severity":1,"range":{"start":{"line":2}},"message":"The term I has type True."},
                    {"severity":1,"range":{"start":{"line":4}},"message":{"value":"Expected False."}},
                    {"severity":1,"range":{"start":{}},"message":"No start line"},
                    {"severity":1,"range":{},"message":"No start position"},
                    {"severity":1,"message":{}},
                    {"severity":1},
                    {"severity":2,"message":"A warning is not an execution error."},
                    {"message":"Missing severity is not an execution error."}
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(diagnostics.uri, "file:///proof.v");
        assert_eq!(diagnostics.version, Some(4_294_967_296));
        assert_eq!(diagnostics.errors(), [
            "Line 3: The term I has type True.",
            "Line 5: Expected False.",
            "No start line",
            "No start position",
            "Rocq reported an error",
            "Rocq reported an error",
        ],);
    }

    #[test]
    fn diagnostic_lines_keep_unsigned_saturating_semantics() {
        let diagnostics: Diagnostics = serde_json::from_str(
            r#"{
                "uri":"file:///proof.v",
                "diagnostics":[
                    {"severity":1,"range":{"start":{"line":18446744073709551615}},"message":"maximum"},
                    {"severity":1,"range":{"start":{"line":-1}},"message":"negative"},
                    {"severity":1,"range":{"start":{"line":1.0}},"message":"fractional"},
                    {"severity":1,"range":{"start":{"line":"2"}},"message":"string"},
                    {"severity":1,"range":{"start":{"line":null}},"message":"null"}
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(diagnostics.version, None);
        assert_eq!(diagnostics.errors(), [
            "Line 18446744073709551615: maximum",
            "negative",
            "fractional",
            "string",
            "null"
        ],);
    }

    #[test]
    fn unusable_diagnostic_messages_keep_the_existing_fallback() {
        let diagnostics: Diagnostics = serde_json::from_str(
            r#"{
                "uri":"file:///proof.v",
                "diagnostics":[
                    {"severity":1,"message":null},
                    {"severity":1,"message":23},
                    {"severity":1,"message":[]},
                    {"severity":1,"message":{"value":false}},
                    {"severity":1,"message":{"value":{"value":"not a direct string"}}}
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(diagnostics.errors(), vec!["Rocq reported an error"; 5]);
    }
}
