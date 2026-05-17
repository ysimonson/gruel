; Tree-sitter folds for Gruel (ADR-0090 Phase 7).
;
; Captures nodes whose content the editor can collapse into a one-line
; summary. Matches the conventional `@fold` capture name used by Helix
; and nvim-treesitter's folding plugin.

[
  (block)
  (struct_body)
  (enum_body)
  (interface_body)
  (derive_body)
] @fold
