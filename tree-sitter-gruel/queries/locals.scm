; Tree-sitter locals query for Gruel (ADR-0090 Phase 7).
;
; The `locals.scm` query lets editors compute scope info — used by
; rainbow-parens plugins, "go to definition" in plain-text-mode tools,
; and the highlights query's `@local.reference` priority lifts.

; ----- scopes ----------------------------------------------------------------

(block) @local.scope
(function_definition) @local.scope
(method_definition) @local.scope
(anonymous_function_expression) @local.scope
(match_arm) @local.scope
(if_expression) @local.scope
(while_expression) @local.scope
(for_expression) @local.scope
(loop_expression) @local.scope

; ----- definitions -----------------------------------------------------------

(let_statement
  pattern: (identifier_pattern (identifier) @local.definition.var))

(parameter
  name: (identifier) @local.definition.parameter)

(function_definition
  name: (identifier) @local.definition.function)

(method_definition
  name: (identifier) @local.definition.function)

(struct_declaration
  name: (identifier) @local.definition.type)

(enum_declaration
  name: (identifier) @local.definition.type)

(interface_declaration
  name: (identifier) @local.definition.type)

(const_declaration
  name: (identifier) @local.definition.const)

(for_expression
  pattern: (identifier_pattern (identifier) @local.definition.var))

; ----- references ------------------------------------------------------------

(identifier) @local.reference
