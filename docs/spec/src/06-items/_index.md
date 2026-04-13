+++
title = "Items"
weight = 6
sort_by = "weight"
template = "spec/section.html"
page_template = "spec/page.html"
+++

# Items

This chapter describes items in Gruel.

{{ rule(id="6.0:1") }}

Items are top-level definitions in a program. Unlike statements, items are visible throughout the module.

## Type Name Uniqueness

{{ rule(id="6.0:2", cat="legality-rule") }}

User-defined type names (structs and enums) **MUST** be unique within a program. Defining multiple types with the same name produces a compile-time error.

{{ rule(id="6.0:3", cat="legality-rule") }}

User-defined types **MUST NOT** use names reserved for built-in types. Currently, the only reserved type name is `String`.

{{ rule(id="6.0:4", cat="example") }}

```gruel
// Error: cannot define type with reserved name
struct String { data: i32 }  // compile error
```
