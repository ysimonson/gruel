# Gruel

Gruel is a programming language. It's like
[rue](https://github.com/rue-language/rue) but worse.

## Why Fork

I think the bones of a really compelling language are there with Rue. But the
project is not contributor-friendly. This is by design: Steve wants to
experiment largely in isolation with some ideas, and it's apparent with e.g.
PRs that are not getting accepted. To make it contributor friendly, and
hopefully iterate on a language faster, this fork gets rid of as much
off-the-beaten path infrastructure as possible in favor of more pedestrian
variants: git instead of jj, just ADRs without beads workflows, cargo instead
of buck, etc.

## Differences from Rue

### Language

- Stabilized anonymous struct methods
- Struct destructuring with partial move ban (ADR-0036)
- Enums with struct variants (ADR-0037), pattern matching (ADR-0038), and anonymous types with comptime generics support (ADR-0039)
- Significantly expanded comptime capabilities (ADR-0040)

### Language Tooling

- Replaced custom x86-64 and AArch64 backends with LLVM
- Removed the custom linker in favor of the system linker (`cc`)
- Replaced inline syscalls with libc-based platform abstraction

### Infrastructure

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
