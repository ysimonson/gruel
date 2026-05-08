+++
title = "Integer Overflow"
weight = 1
template = "spec/page.html"
+++

# Integer Overflow

{{ rule(id="8.1:1", cat="dynamic-semantics") }}

Integer arithmetic that overflows the representable range of its result type **MUST** wrap around modulo 2^N, where N is the bit width of the type. The result is the unique value in the type's range that is congruent to the mathematical result modulo 2^N.

{{ rule(id="8.1:2", cat="dynamic-semantics") }}

Integer overflow does not cause a runtime panic and does not abort the program.

{{ rule(id="8.1:3", cat="normative") }}

The following operations wrap on overflow:
- Addition (`+`)
- Subtraction (`-`)
- Multiplication (`*`)
- Negation (`-` unary)

{{ rule(id="8.1:4") }}

```gruel
fn main() -> i32 {
    2147483647 + 1  // wraps to -2147483648
}
```

{{ rule(id="8.1:5") }}

```gruel
fn main() -> i32 {
    -2147483648 - 1  // wraps to 2147483647
}
```

{{ rule(id="8.1:6") }}

Future versions of Gruel may provide checked or saturating arithmetic operations as alternatives to the default wrapping semantics.
