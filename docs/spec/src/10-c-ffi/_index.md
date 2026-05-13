+++
title = "C Foreign Function Interface"
weight = 10
sort_by = "weight"
template = "spec/section.html"
page_template = "spec/page.html"
+++

# C Foreign Function Interface

This chapter describes Gruel's surface for calling C and being called from C, per ADR-0085.

{{ rule(id="10.0:1") }}

C FFI is gated behind the `c_ffi` preview feature. Two surface constructs participate: the `@mark(c)` marker (applicable to fns and structs) and the `link_extern("…") { … }` block form.
