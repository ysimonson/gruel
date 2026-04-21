+++
title = "Floating-Point Types"
weight = 11
template = "spec/page.html"
+++

# Floating-Point Types

{{ rule(id="3.11:1", cat="normative") }}

A floating-point type is one of: `f16`, `f32`, `f64`, or `f128`.

{{ rule(id="3.11:2", cat="normative") }}

The type `f16` is an IEEE 754 binary16 half-precision floating-point type occupying 2 bytes with 2-byte alignment.

{{ rule(id="3.11:3", cat="normative") }}

The type `f32` is an IEEE 754 binary32 single-precision floating-point type occupying 4 bytes with 4-byte alignment.

{{ rule(id="3.11:4", cat="normative") }}

The type `f64` is an IEEE 754 binary64 double-precision floating-point type occupying 8 bytes with 8-byte alignment.

{{ rule(id="3.11:5", cat="normative") }}

The type `f128` is an IEEE 754 binary128 quad-precision floating-point type occupying 16 bytes with 16-byte alignment.

{{ rule(id="3.11:6", cat="normative") }}

Floating-point types are copy types.

{{ rule(id="3.11:7", cat="normative") }}

A floating-point literal is a sequence of digits containing a decimal point or an exponent. The literal `42.0` is a floating-point literal; `42` without a decimal point is an integer literal.

{{ rule(id="3.11:8", cat="normative") }}

An unqualified floating-point literal (one without an explicit type annotation) has type `f64` by default.

{{ rule(id="3.11:9", cat="normative") }}

A floating-point literal can be assigned to any floating-point type variable via type inference. The literal value is narrowed to the target type's precision during code generation.
