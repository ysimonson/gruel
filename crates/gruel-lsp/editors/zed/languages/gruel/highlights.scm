; Tree-sitter highlights for Gruel.
;
; Mirrors crates/tree-sitter-gruel/queries/highlights.scm. Capture names
; follow the helix / nvim-treesitter / Zed vocabulary.

; ----- keywords --------------------------------------------------------------

[
  "fn"
  "let"
  "mut"
  "return"
  "if"
  "else"
  "while"
  "for"
  "in"
  "loop"
  "match"
  "struct"
  "enum"
  "interface"
  "derive"
  "const"
  "comptime"
  "comptime_unroll"
  "checked"
  "link_extern"
  "static_link_extern"
] @keyword

(visibility) @keyword.modifier

(break_expression) @keyword.control.return
(continue_expression) @keyword.control.return
(self_literal) @variable.builtin
(self_type_literal) @type.builtin

; ----- punctuation -----------------------------------------------------------

[
  "("
  ")"
  "["
  "]"
  "{"
  "}"
] @punctuation.bracket

[
  ","
  ";"
  ":"
  "::"
  "."
  "=>"
  "->"
] @punctuation.delimiter

; ----- operators -------------------------------------------------------------

[
  "+"
  "-"
  "*"
  "/"
  "%"
  "="
  "=="
  "!="
  "<"
  ">"
  "<="
  ">="
  "&&"
  "||"
  "!"
  "&"
  "|"
  "^"
  "~"
  "<<"
  ">>"
] @operator

; ----- literals --------------------------------------------------------------

(integer_literal) @number
(float_literal) @number.float
(boolean_literal) @constant.builtin.boolean
(unit_literal) @constant.builtin
(string_literal) @string
(escape_sequence) @string.escape
(char_literal) @character

; ----- comments --------------------------------------------------------------

(line_comment) @comment
(doc_comment) @comment.documentation

; ----- types -----------------------------------------------------------------

(primitive_type) @type.builtin
(self_type) @type.builtin
(named_type (identifier) @type)

; ----- definitions -----------------------------------------------------------

(function_definition name: (identifier) @function)
(method_definition name: (identifier) @function.method)
(interface_method name: (identifier) @function.method)
(extern_function_declaration name: (identifier) @function)

(struct_declaration name: (identifier) @type)
(enum_declaration name: (identifier) @type)
(interface_declaration name: (identifier) @type)
(derive_declaration name: (identifier) @type)

(enum_variant name: (identifier) @constructor)

(parameter name: (identifier) @variable.parameter)

; ----- references ------------------------------------------------------------

(call_expression
  function: (identifier) @function.call)

(method_call_expression
  method: (identifier) @function.method.call)

(intrinsic_call_expression
  name: (identifier) @function.builtin)
"@" @function.builtin

(directive
  name: (identifier) @attribute)

(field_expression
  field: (identifier) @variable.member)
