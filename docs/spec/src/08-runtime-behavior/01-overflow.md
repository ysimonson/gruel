+++
title = "Integer Overflow"
weight = 1
template = "spec/page.html"
+++

# Integer Overflow

{{ rule(id="8.1:1", cat="dynamic-semantics") }}

Integer overflow during arithmetic operations **MUST** cause a runtime panic.

{{ rule(id="8.1:2", cat="dynamic-semantics") }}

On overflow, the program **MUST** terminate with exit code 101 and print an error message.

{{ rule(id="8.1:3", cat="normative") }}

The following operations **MAY** overflow:
- Addition (`+`)
- Subtraction (`-`)
- Multiplication (`*`)
- Negation (`-` unary)

{{ rule(id="8.1:4") }}

```gruel
fn main() -> i32 {
    2147483647 + 1  // Runtime error: integer overflow
}
```

{{ rule(id="8.1:5") }}

```gruel
fn main() -> i32 {
    -2147483648 - 1  // Runtime error: integer overflow
}
```

{{ rule(id="8.1:6") }}

Future versions of Gruel may provide wrapping arithmetic operations that do not panic on overflow.
