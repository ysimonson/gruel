+++
title = "Floating-Point Types"
weight = 11
template = "spec/page.html"
+++

# Floating-Point Types

{{ rule(id="3.11:1", cat="normative") }}

A floating-point type is one of: `f16`, `f32`, or `f64`.

{{ rule(id="3.11:2", cat="normative") }}

The type `f16` is an IEEE 754 binary16 half-precision floating-point type occupying 2 bytes with 2-byte alignment.

{{ rule(id="3.11:3", cat="normative") }}

The type `f32` is an IEEE 754 binary32 single-precision floating-point type occupying 4 bytes with 4-byte alignment.

{{ rule(id="3.11:4", cat="normative") }}

The type `f64` is an IEEE 754 binary64 double-precision floating-point type occupying 8 bytes with 8-byte alignment.

{{ rule(id="3.11:5", cat="normative") }}

Floating-point types are copy types.

{{ rule(id="3.11:6", cat="normative") }}

A floating-point literal is a sequence of digits containing a decimal point or an exponent. The literal `42.0` is a floating-point literal; `42` without a decimal point is an integer literal.

{{ rule(id="3.11:7", cat="normative") }}

An unqualified floating-point literal (one without an explicit type annotation) has type `f64` by default.

{{ rule(id="3.11:8", cat="normative") }}

A floating-point literal can be assigned to any floating-point type variable via type inference. The literal value is narrowed to the target type's precision during code generation.

{{ rule(id="3.11:9", cat="normative") }}

The arithmetic operators `+`, `-`, `*`, `/`, and `%` are defined for floating-point types. Both operands must have the same floating-point type, and the result has that type.

{{ rule(id="3.11:10", cat="normative") }}

The comparison operators `==`, `!=`, `<`, `>`, `<=`, and `>=` are defined for floating-point types. Both operands must have the same floating-point type, and the result has type `bool`. Comparisons use ordered semantics: if either operand is NaN, the result is `false` for all operators except `!=`, which returns `true`.

{{ rule(id="3.11:11", cat="normative") }}

The unary negation operator `-` is defined for floating-point types. The operand and result have the same floating-point type.

{{ rule(id="3.11:12", cat="legality-rule") }}

Bitwise operators (`&`, `|`, `^`, `<<`, `>>`, `~`) are not defined for floating-point types. Using a bitwise operator on a floating-point operand is a compile-time error.
