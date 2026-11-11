use std::collections::BTreeSet;

use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

#[test]
fn rocq_queries_load_and_highlight_proof_endings() {
    let language = tree_sitter_rocq::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).unwrap();
    let source = b"Lemma checked : True.\nProof. exact I. Qed.\nLemma admitted : True.\nAdmitted.\nDefinition transparent : True.\nProof. exact I. Defined.\nGoal True.\nAbort.\n";
    let tree = parser.parse(source, None).unwrap();
    assert!(!tree.root_node().has_error());
    let mut keywords = BTreeSet::new();
    for (name, text) in [
        ("highlights", include_str!("../languages/rocq/highlights.scm")),
        ("outline", include_str!("../languages/rocq/outline.scm")),
        ("brackets", include_str!("../languages/rocq/brackets.scm")),
        ("indents", include_str!("../languages/rocq/indents.scm")),
    ] {
        let query = Query::new(&language, text).unwrap_or_else(|error| panic!("Rocq {name} query must load: {error}"));
        if name == "highlights" {
            let mut cursor = QueryCursor::new();
            let mut captures = cursor.captures(&query, tree.root_node(), source.as_slice());
            while let Some((matched, index)) = captures.next() {
                let capture = matched.captures[*index];
                if query.capture_names()[capture.index as usize] == "keyword" {
                    keywords.insert(capture.node.utf8_text(source).unwrap());
                }
            }
        }
    }
    for ending in ["Qed", "Admitted", "Defined", "Abort"] {
        assert!(keywords.contains(ending), "{ending} must have keyword highlighting");
    }
}
