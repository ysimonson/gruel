# Gruel

Gruel is a programming language. It's like
[rue](https://github.com/rue-language/rue) but worse.

## Differences from Rue

### Language

Theme: Build upon Rue's bones, keeping a lot of the broad ideas but making breaking changes to maintain cohesion as it evolves.

- Added for loops and first class `Range`s (ADR-0041)
- Several new numeric primitives - `f16`/`f32`/`f64` and `isize`/`usize` (ADR-0046)
- Use of `usize` for indexing (ADR-0054)
- Tuples (ADR-0048)
- Slices (ADR-0064)
- Canonical `Option(T)` and `Clone` interface (ADR-0065)
- Vecs (ADR-0066)
- Anonymous functions (ADR-0055)
- Go-like structurally typed interfaces (ADR-0056, ADR-0057, ADR-0060)
- Rust-like `@derive` for structs and enums (ADR-0058)
- Reworked pointers and references to a less keyword-driven syntax to accomodate the growing number of variants (ADR-0061, ADR-0062, ADR-0063)
- Stabilized anonymous struct methods (ADR-0029)
- All struct/enum methods, including destructors, are inlined - there are no `impl` blocks (ADR-0053)
- Added struct destructuring (ADR-0036)
- Banned partial moves to simplify analysis, especially with linear types (ADR-0036)
- Enums with struct variants (ADR-0037), pattern matching (ADR-0038), and anonymous types with comptime generics support (ADR-0039)
- Significantly expanded comptime capabilities (ADR-0040, ADR-0042, ADR-0045)
- Significantly expanded pattern matching and destructuring capabilities (ADR-0049, ADR-0051, ADR-0052)

### Language Tooling

Theme: rely on LLVM / libc to provide better performance and more diverse platform support.

- Replaced custom x86-64 and AArch64 backends with LLVM (ADR-0033)
- Removed the custom linker in favor of the system linker LLVM (ADR-0033)
- Replaced inline syscalls with libc-based platform abstraction (ADR-0035)
- Significantly expanded benchmarks to track comptime and runtime performance (ADR-0043)

### Infrastructure

Theme: get rid of as much off-the-beaten-path infrastructure as possible in favor of more pedestrian variants: git instead of jj, just ADRs without beads workflows,
cargo instead of buck, etc. The goal is to make this more contributor-friendly.

- Replaced jj with git
- Replaced buck2 with cargo and vendored deps with crates.io
- Removed reindeer (buck2 cargo integration)
- Migrated fuzz testing from a custom harness to cargo-fuzz
- Reworked fuzzer to work with the new LLVM backend and take advantage of expanded comptime capabilities
- Removed beads issue tracking in favor of plain ADRs
- Shorter mangled symbol names to support macOS's default linker
- Added a Makefile as the primary build/test entrypoint
- Added a spec test cache to run the full test suite quicker

## License

Licensed under either of

 * Apache License, Version 2.0
   ([LICENSE-APACHE](LICENSE-APACHE) or http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license
   ([LICENSE-MIT](LICENSE-MIT) or http://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in the work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
