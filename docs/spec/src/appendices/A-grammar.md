+++
title = "Grammar"
weight = 1
template = "spec/page.html"
+++

# Appendix A: Grammar

This appendix contains the complete EBNF grammar for Gruel.

```ebnf
(* Program structure *)
program        = { item } ;
item           = [ directive ] ( function | struct_def | enum_def | impl_block | drop_fn ) ;

(* Builtins *)
directive      = "@" IDENT "(" [ directive_args ] ")" ;
directive_args = directive_arg { "," directive_arg } ;
directive_arg  = IDENT ;
intrinsic      = "@" IDENT "(" [ intrinsic_args ] ")" ;
intrinsic_args = intrinsic_arg { "," intrinsic_arg } ;
intrinsic_arg  = expression | type ;

(* Functions *)
function       = "fn" IDENT "(" [ params ] ")" [ "->" type ] "{" block "}" ;
params         = param { "," param } ;
param          = [ param_mode ] IDENT ":" type ;
param_mode     = "inout" | "borrow" ;
block          = { statement } [ expression ] ;

(* Structs *)
struct_def     = "struct" IDENT "{" [ struct_fields ] "}" ;
struct_fields  = struct_field { "," struct_field } [ "," ] ;
struct_field   = IDENT ":" type ;

(* Enums *)
enum_def       = "enum" IDENT "{" [ enum_variants ] "}" ;
enum_variants  = enum_variant { "," enum_variant } [ "," ] ;
enum_variant   = IDENT [ variant_fields ] ;
variant_fields = "(" type_list ")"
               | "{" struct_variant_fields "}" ;
type_list      = type { "," type } ;
struct_variant_fields = struct_variant_field { "," struct_variant_field } [ "," ] ;
struct_variant_field  = IDENT ":" type ;

(* Impl blocks *)
impl_block     = "impl" IDENT "{" { method } "}" ;
method         = "fn" IDENT "(" [ "self" [ "," params ] | params ] ")" [ "->" type ] "{" block "}" ;

(* Destructors *)
drop_fn        = "drop" "fn" IDENT "(" "self" ")" "{" block "}" ;

(* Statements *)
statement      = [ directive ] ( let_stmt | assign_stmt | expr_stmt ) ;
let_stmt       = "let" [ "mut" ] IDENT [ ":" type ] "=" expression ";" ;
assign_stmt    = IDENT "=" expression ";"
               | IDENT "[" expression "]" "=" expression ";"
               | expression "." IDENT "=" expression ";" ;
expr_stmt      = expression ";" ;

(* Types *)
type           = "i8" | "i16" | "i32" | "i64"
               | "u8" | "u16" | "u32" | "u64"
               | "bool" | "()"
               | "[" type ";" INTEGER "]"
               | IDENT ;

(* Expressions *)
expression     = or_expr ;
or_expr        = and_expr { "||" and_expr } ;
and_expr       = comparison { "&&" comparison } ;
comparison     = bitor_expr { ( "==" | "!=" | "<" | ">" | "<=" | ">=" ) bitor_expr } ;
bitor_expr     = bitxor_expr { "|" bitxor_expr } ;
bitxor_expr    = bitand_expr { "^" bitand_expr } ;
bitand_expr    = shift_expr { "&" shift_expr } ;
shift_expr     = additive { ( "<<" | ">>" ) additive } ;
additive       = multiplicative { ( "+" | "-" ) multiplicative } ;
multiplicative = unary { ( "*" | "/" | "%" ) unary } ;
unary          = "-" unary | "!" unary | "~" unary | postfix ;
postfix        = primary { "[" expression "]" | "(" [ args ] ")" | "." IDENT } ;
args           = arg { "," arg } ;
arg            = [ param_mode ] expression ;
primary        = INTEGER | BOOL | IDENT | enum_variant_expr
               | "(" expression ")"
               | block_expr
               | if_expr
               | match_expr
               | while_expr
               | loop_expr
               | for_expr
               | "break" | "continue"
               | return_expr
               | array_literal
               | struct_literal
               | intrinsic ;
enum_variant_expr = IDENT "::" IDENT [ "(" [ args ] ")" | "{" [ field_inits ] "}" ] ;

(* Compound expressions *)
block_expr     = "{" block "}" ;
if_expr        = "if" expression "{" block "}" [ else_clause ] ;
else_clause    = "else" ( "{" block "}" | if_expr ) ;
match_expr     = "match" expression "{" { match_arm "," } [ match_arm ] "}" ;
match_arm      = pattern "=>" expression ;
pattern        = "_" | INTEGER | BOOL | enum_variant_pattern ;
enum_variant_pattern = IDENT "::" IDENT [ "(" [ bindings ] ")" | "{" [ field_patterns ] "}" ] ;
bindings       = binding { "," binding } ;
binding        = "_" | [ "mut" ] IDENT ;
field_patterns = field_pattern { "," field_pattern } [ "," ] ;
field_pattern  = IDENT ":" binding
               | IDENT ;
while_expr     = "while" expression "{" block "}" ;
loop_expr      = "loop" "{" block "}" ;
for_expr       = "for" [ "mut" ] IDENT "in" expression "{" block "}" ;
return_expr    = "return" expression ;
array_literal  = "[" [ expression { "," expression } ] "]" ;
struct_literal = IDENT "{" [ field_inits ] "}" ;
field_inits    = field_init { "," field_init } [ "," ] ;
field_init     = IDENT ":" expression
               | IDENT ;

(* Lexical elements *)
IDENT          = ( letter | "_" ) { letter | digit | "_" } ;
INTEGER        = digit { digit } ;
BOOL           = "true" | "false" ;
letter         = "a" | ... | "z" | "A" | ... | "Z" ;
digit          = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" ;

(* Whitespace and comments are ignored between tokens *)
whitespace     = " " | "\t" | "\n" | "\r" ;
line_comment   = "//" { any_char_except_newline } newline ;
```
