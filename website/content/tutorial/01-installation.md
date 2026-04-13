+++
title = "Installation"
weight = 1
template = "tutorial/page.html"
+++

# Installation

Gruel is currently in early development. To try it out, you'll need to build from source. If you do try it out, you'll certainly find bugs, and if you do please [file them](https://github.com/gruel-language/gruel/issues)!

## Prerequisites

- [dotslash](https://dotslash-cli.com) - Used to bootstrap Buck2 and the Rust toolchain
- `clang` - Used as the linker (install via your system package manager)

The repository includes everything else: Buck2 is bootstrapped via dotslash, and a hermetic Rust toolchain is downloaded automatically on first build.

## Building from Source

```bash
git clone https://github.com/gruel-language/gruel
cd gruel
./buck2 build //crates/gruel:gruel
```

The first build will download the Rust toolchain, which may take a minute. Subsequent builds are fast.

That's it! You now have a working Gruel compiler. In the next chapter, we'll write our first program.
