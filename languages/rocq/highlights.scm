; Original queries for this extension, licensed under MIT.
(comment) @comment
(string) @string
[(number) (integer) (decnat) (hexnat)] @number
(ident) @variable
(dotted_qualid) @variable
(ident_decl (ident) @function)
(inductive_definition name: (ident) @type)
(constructor name: (ident) @constructor)
(attribute (ident) @attribute)
(metavariable) @variable.special
(wildcard) @variable.special
(bullet) @punctuation.special
; Qed and Admitted are named command nodes in the pinned grammar.
[(qed_command) (admitted_command)] @keyword

[
  "Require" "Import" "From" "Export"
  "Theorem" "Lemma" "Fact" "Remark" "Corollary" "Proposition" "Property"
  "Definition" "Example" "Fixpoint" "Inductive"
  "Axiom" "Axioms" "Conjecture" "Conjectures" "Parameter" "Parameters"
  "Hypothesis" "Hypotheses" "Variable" "Variables"
  "Section" "Module" "End" "Proof" "Defined" "Abort"
  "Check" "Compute" "Print" "Search" "Locate"
  "Set" "Unset" "Arguments" "Notation" "Reserved" "Open" "Close" "Scope"
  "with"
] @keyword

["(" ")" "[" "]" "{" "}"] @punctuation.bracket
["." "..." ":" ";" ","] @punctuation.delimiter
[":=" "=>" "->" "<-"] @operator
