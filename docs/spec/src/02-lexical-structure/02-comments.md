+++
title = "Comments"
weight = 2
template = "spec/page.html"
+++

# Comments

{{ rule(id="2.2:1", cat="normative") }}

Line comments begin with `//` and extend to the end of the line.

```ebnf
line_comment = "//" { any_char_except_newline } newline ;
```

{{ rule(id="2.2:2", cat="normative") }}

Non-doc comments are discarded during lexical analysis and do not affect program semantics. Doc comments (§2.2:5) are surfaced onto the AST and do not affect runtime behaviour either, but their text is available to downstream tooling.

{{ rule(id="2.2:3") }}

```gruel
// This is a comment
fn main() -> i32 {
    42  // This is also a comment
}
```

{{ rule(id="2.2:4") }}

Block comments (`/* ... */`) are not currently supported.

{{ rule(id="2.2:5", cat="normative") }}

A line beginning with exactly three forward slashes (`///`) followed by any sequence of characters up to the end of the line introduces a *doc comment*. Four or more consecutive forward slashes (e.g. `////`) are still ordinary line comments. The body of a doc comment is the text after the marker with at most one single leading space removed.

```ebnf
doc_line = "///" { any_char_except_newline } newline ;
```

{{ rule(id="2.2:6", cat="normative") }}

A run of consecutive `doc_line`s, with no blank line and no non-doc token between them, forms a *doc block*. A blank line, a non-doc token, or end of file terminates the run.

{{ rule(id="2.2:7", cat="normative") }}

A doc block qualifies as the *module candidate* iff it is the textually first doc block in the file and no item appears above it in the same file. A qualifying block separated by at least one blank line from the next item attaches to the module; a qualifying block glued to the next item (no blank line between) attaches to that item.

{{ rule(id="2.2:8", cat="legality-rule") }}

A doc block that does not qualify as the module candidate must be immediately followed (no blank line between) by an item; otherwise it is a parse error.

{{ rule(id="2.2:9") }}

```gruel
/// Module-level documentation.

/// Documentation attached to `main`.
fn main() -> i32 {
    42
}
```
