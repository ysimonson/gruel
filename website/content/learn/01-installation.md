+++
title = "Installation"
weight = 1
template = "learn/page.html"
+++

# Installation

Gruel is currently in early development. To try it out, you'll need to build from source. If you do try it out, you'll certainly find bugs, and if you do please [file them](https://github.com/ysimonson/gruel/issues)!

## Prerequisites

- [Rust toolchain](https://rustup.rs) - Install via rustup
- **LLVM 22** - The compiler backend requires LLVM
  - **macOS**: `brew install llvm@22` and ensure `llvm-config` is on your `PATH`
  - **Ubuntu/Debian**: `apt install llvm-22-dev`
  - **Fedora**: `dnf install llvm22-devel`
- A system linker
  - **macOS**: `clang` (install via `xcode-select --install`)
  - **Linux**: `gcc` or `clang`

## Building from Source

```bash
git clone https://github.com/ysimonson/gruel
cd gruel
cargo build -p gruel --release
```

The first build will compile all dependencies, which may take a minute. Subsequent builds are fast.

That's it! You now have a working Gruel compiler at `target/release/gruel`. In the next chapter, we'll write our first program.
