; Tree-sitter indents for Gruel (ADR-0090 Phase 7).
;
; The `@indent.begin` / `@indent.end` query is consumed by Helix and
; tree-sitter-indent integrations. Nodes captured with `@indent.begin`
; introduce a new indentation level; `@indent.end` pops one.

[
  (block)
  (struct_body)
  (enum_body)
  (interface_body)
  (derive_body)
  (parameter_list)
] @indent.begin

[
  "}"
  ")"
  "]"
] @indent.end
