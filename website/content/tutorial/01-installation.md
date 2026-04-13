+++
title = "Installation"
weight = 1
template = "tutorial/page.html"
+++

# Installation

Gruel is currently in early development. To try it out, you'll need to build from source. If you do try it out, you'll certainly find bugs, and if you do please [file them](https://github.com/gruel-language/gruel/issues)!

## Prerequisites

- [Rust toolchain](https://rustup.rs) - Install via rustup
- `clang` - Used as the linker on macOS (install via `xcode-select --install`)

## Building from Source

```bash
git clone https://github.com/gruel-language/gruel
cd gruel
cargo build -p gruel --release
```

The first build will compile all dependencies, which may take a minute. Subsequent builds are fast.

That's it! You now have a working Gruel compiler at `target/release/gruel`. In the next chapter, we'll write our first program.
