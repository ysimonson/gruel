; Outline view: items shown in Zed's symbols panel and breadcrumb.

(function_definition
  "fn" @context
  name: (identifier) @name) @item

(extern_function_declaration
  "fn" @context
  name: (identifier) @name) @item

(struct_declaration
  "struct" @context
  name: (identifier) @name) @item

(enum_declaration
  "enum" @context
  name: (identifier) @name) @item

(interface_declaration
  "interface" @context
  name: (identifier) @name) @item

(const_declaration
  "const" @context
  name: (identifier) @name) @item

(method_definition
  "fn" @context
  name: (identifier) @name) @item

(interface_method
  "fn" @context
  name: (identifier) @name) @item
