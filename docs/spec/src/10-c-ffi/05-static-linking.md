+++
title = "Static linking"
weight = 5
template = "spec/page.html"
+++

# Static linking (ADR-0086)

{{ rule(id="10.5:1", cat="normative") }}
The `static_link_extern("name") { … }` keyword introduces a sibling form to `link_extern` that requests static linkage for the named library. Body grammar (items, body-less fn declarations, implicit `@mark(c)`, `@link_name` overrides, empty-block permission) is identical to `link_extern`. The keyword is gated behind the `c_ffi_extras` preview feature.

{{ rule(id="10.5:2", cat="normative") }}
A given library name cannot be declared with both `link_extern` (dynamic) and `static_link_extern` (static) linkage across the same compilation unit. Mixed declarations are rejected with a compile-time diagnostic.

{{ rule(id="10.5:3", cat="normative") }}
On ELF targets, a static-linked library contributes `-Wl,-Bstatic -l<name> -Wl,-Bdynamic` to the linker line. Static libraries are emitted in lex-sorted order ahead of dynamic ones; the closing `-Wl,-Bdynamic` ensures libc and the Gruel runtime remain dynamic.

{{ rule(id="10.5:4", cat="normative") }}
On Mach-O targets, a static-linked library contributes `-Wl,-search_paths_first -l<name>`. macOS `ld` has no `-Bstatic`/`-Bdynamic` toggle; `-search_paths_first` causes the linker to scan a search directory for `lib<name>.a` before `lib<name>.dylib`. If only a `.dylib` is present, the link succeeds dynamically — same outcome as the ELF dynamic fallback. A diagnostic warning is reserved for this fallback but is not emitted in this revision.

{{ rule(id="10.5:5", cat="normative") }}
`static_link_extern` blocks may not nest. Mixing `static_link_extern` inside `link_extern` (or vice versa) is rejected with the same `LinkExternNested` diagnostic ADR-0085 introduced for `link_extern` nesting.

{{ rule(id="10.5:6", cat="example") }}
```gruel
static_link_extern("foo") {
    fn foo_init() -> c_int;
}

link_extern("c") { // dynamic, unchanged from ADR-0085
    fn write(fd: c_int, buf: Ptr(u8), n: usize) -> isize;
}
```
