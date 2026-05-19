; Zed indents query. Nodes captured with @indent introduce a new level;
; tokens captured with @end pop one back.

[
  (block)
  (struct_body)
  (enum_body)
  (interface_body)
  (derive_body)
  (parameter_list)
] @indent

[
  "}"
  ")"
  "]"
] @end
