.PHONY: test quick-test fmt fmt-check check bench website website-serve website-deploy \
        fuzz fuzz-lexer fuzz-parser fuzz-compiler \
        fuzz-structured-compiler fuzz-structured-invalid \
        fuzz-comptime-differential \
        claude

# Detect LLVM 22 on macOS (Homebrew). Set LLVM_SYS_221_PREFIX if not already set.
# On Linux, set LLVM_SYS_221_PREFIX=/usr/lib/llvm-22 or let llvm-sys find it via llvm-config.
LLVM22_BREW := /opt/homebrew/opt/llvm@22
ifeq ($(shell test -d $(LLVM22_BREW) && echo yes),yes)
  export LLVM_SYS_221_PREFIX ?= $(LLVM22_BREW)
endif

# Run unit tests only (fast feedback during development).
quick-test:
	cargo test --workspace --exclude gruel-runtime

# Run all tests (unit + spec + traceability + UI tests).
# Pass ARGS="pattern" to filter spec/UI tests, e.g.: make test ARGS="1.1"
test: quick-test
	cargo build -p gruel
	GRUEL_BINARY=target/debug/gruel \
	GRUEL_SPEC_CASES=crates/gruel-spec/cases \
	cargo run -p gruel-spec -- --quiet $(ARGS)
	GRUEL_SPEC_DIR=docs/spec/src \
	GRUEL_SPEC_CASES=crates/gruel-spec/cases \
	cargo run -p gruel-spec -- --traceability
	GRUEL_BINARY=target/debug/gruel \
	GRUEL_UI_CASES=crates/gruel-ui-tests/cases \
	cargo run -p gruel-ui-tests -- --quiet $(ARGS)

# Format all Rust files.
fmt:
	cargo fmt --all

# Check formatting without making changes (for CI).
check:
	cargo check --workspace --all-targets --exclude gruel-runtime
	cargo check --manifest-path fuzz/Cargo.toml --all-targets
	cargo clippy --workspace --all-targets --exclude gruel-runtime
	cargo fmt --all -- --check

# Run benchmarks. Pass ARGS="--iterations 10" etc. to forward options.
bench:
	./bench.sh $(ARGS)

# Run all fuzz targets for FUZZ_TIME seconds each (default: 60).
# Pass FUZZ_TIME=300 for a longer run.
FUZZ_TIME ?= 60
fuzz: fuzz-lexer fuzz-parser fuzz-compiler \
      fuzz-structured-compiler fuzz-structured-invalid \
      fuzz-comptime-differential

fuzz-lexer:
	cargo +nightly fuzz run lexer -- -max_total_time=$(FUZZ_TIME)

fuzz-parser:
	cargo +nightly fuzz run parser -- -max_total_time=$(FUZZ_TIME)

fuzz-compiler:
	cargo +nightly fuzz run compiler -- -max_total_time=$(FUZZ_TIME)

fuzz-structured-compiler:
	cargo +nightly fuzz run structured_compiler -- -max_total_time=$(FUZZ_TIME)

fuzz-structured-invalid:
	cargo +nightly fuzz run structured_invalid -- -max_total_time=$(FUZZ_TIME)

fuzz-comptime-differential:
	cargo +nightly fuzz run comptime_differential -- -max_total_time=$(FUZZ_TIME)

# Run Claude in a sandboxed environment.
claude:
	vibe --send "ln -fs ~/.claude/dot_claude_dot_json_should_have_been_here.json ~/.claude.json" \
	     --send "IS_SANDBOX=1 claude --allow-dangerously-skip-permissions --dangerously-skip-permissions --append-system-prompt='You are running in a sandboxed debian environment, so feel free to install and remove packages as needed, etc. DO NOT destructively rewrite git history or do something crazy like sending an email.'"

# Build the website.
website:
	./website/build.sh

# Run the website dev server at http://127.0.0.1:1111.
website-serve:
	./website/build.sh serve

# Build the website for production deployment.
website-deploy:
	./website/build.sh deploy
