.PHONY: test quick-test fmt fmt-check check bench website website-serve website-deploy \
        fuzz fuzz-lexer fuzz-parser fuzz-compiler fuzz-emitter fuzz-emitter-sequence \
        fuzz-structured-compiler fuzz-structured-invalid fuzz-structured-emitter

# Detect LLVM 18 on macOS (Homebrew). Set LLVM_SYS_180_PREFIX if not already set.
# On Linux, llvm-config-18 is usually in PATH and llvm-sys finds it automatically.
LLVM18_BREW := /opt/homebrew/opt/llvm@18
ifeq ($(shell test -d $(LLVM18_BREW) && echo yes),yes)
  export LLVM_SYS_180_PREFIX ?= $(LLVM18_BREW)
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
	cargo check --workspace --exclude gruel-runtime
	cargo clippy --workspace --exclude gruel-runtime
	cargo fmt --all -- --check

# Run benchmarks. Pass ARGS="--iterations 10" etc. to forward options.
bench:
	./bench.sh $(ARGS)

# Run all fuzz targets for FUZZ_TIME seconds each (default: 60).
# Pass FUZZ_TIME=300 for a longer run.
FUZZ_TIME ?= 60
fuzz: fuzz-lexer fuzz-parser fuzz-compiler fuzz-emitter fuzz-emitter-sequence \
      fuzz-structured-compiler fuzz-structured-invalid fuzz-structured-emitter

fuzz-lexer:
	cargo +nightly fuzz run lexer -- -max_total_time=$(FUZZ_TIME)

fuzz-parser:
	cargo +nightly fuzz run parser -- -max_total_time=$(FUZZ_TIME)

fuzz-compiler:
	cargo +nightly fuzz run compiler -- -max_total_time=$(FUZZ_TIME)

fuzz-emitter:
	cargo +nightly fuzz run emitter -- -max_total_time=$(FUZZ_TIME)

fuzz-emitter-sequence:
	cargo +nightly fuzz run emitter_sequence -- -max_total_time=$(FUZZ_TIME)

fuzz-structured-compiler:
	cargo +nightly fuzz run structured_compiler -- -max_total_time=$(FUZZ_TIME)

fuzz-structured-invalid:
	cargo +nightly fuzz run structured_invalid -- -max_total_time=$(FUZZ_TIME)

fuzz-structured-emitter:
	cargo +nightly fuzz run structured_emitter -- -max_total_time=$(FUZZ_TIME)

# Build the website.
website:
	./website/build.sh

# Run the website dev server at http://127.0.0.1:1111.
website-serve:
	./website/build.sh serve

# Build the website for production deployment.
website-deploy:
	./website/build.sh deploy
