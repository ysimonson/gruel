.PHONY: test quick-test doctest fmt fmt-check check bench crate-docs \
        check-intrinsic-docs gen-intrinsic-docs \
        check-builtins-docs gen-builtins-docs \
        check-spec-builtins \
        gen-stdlib-docs check-stdlib-docs \
        website website-serve website-deploy \
        fuzz fuzz-lexer fuzz-parser fuzz-compiler \
        fuzz-structured-compiler fuzz-structured-invalid \
        fuzz-comptime-differential fuzz-parser-differential \
        tree-sitter-generate tree-sitter-test \
        lsp-diagnostic-differential \
        install-lsp \
        claude

# Detect LLVM 22 on macOS (Homebrew). Set LLVM_SYS_221_PREFIX if not already set.
# On Linux, set LLVM_SYS_221_PREFIX=/usr/lib/llvm-22 or let llvm-sys find it via llvm-config.
LLVM22_BREW := /opt/homebrew/opt/llvm@22
ifeq ($(shell test -d $(LLVM22_BREW) && echo yes),yes)
  export LLVM_SYS_221_PREFIX ?= $(LLVM22_BREW)
endif

# Wall-clock guard for `make test` stages so a hung compiler/test runner can't
# wedge the build forever. Uses GNU `timeout` (Linux) or `gtimeout` (macOS via
# `brew install coreutils`); silently no-ops if neither is installed.
# Override with `make test TEST_TIMEOUT=300` to change the per-stage budget.
TEST_TIMEOUT ?= 600
TIMEOUT_BIN := $(shell command -v timeout 2>/dev/null || command -v gtimeout 2>/dev/null)
ifeq ($(TIMEOUT_BIN),)
  TIMEOUT :=
else
  TIMEOUT := $(TIMEOUT_BIN) --foreground --kill-after=10 $(TEST_TIMEOUT)
endif

# Run unit tests only (fast feedback during development).
# Skips doctests — rustdoc's merged-doctest pass costs ~25s/crate even when
# the crate has none. Run `make doctest` to exercise them.
quick-test:
	$(TIMEOUT) cargo test --workspace --exclude gruel-runtime --lib --bins --tests

# Run doctests across the workspace. Slow (rustdoc per-crate overhead) so it's
# split out from quick-test.
doctest:
	$(TIMEOUT) cargo test --workspace --exclude gruel-runtime --doc

# Run all tests (unit + doctests + spec + traceability + UI tests + examples).
# Pass ARGS="pattern" to filter spec/UI/example tests, e.g.: make test ARGS="1.1"
test: quick-test doctest tree-sitter-differential lsp-diagnostic-differential
	$(TIMEOUT) cargo build -p gruel
	GRUEL_BINARY=target/debug/gruel \
	GRUEL_SPEC_CASES=crates/gruel-spec/cases \
	$(TIMEOUT) cargo run -p gruel-spec -- --quiet $(ARGS)
	GRUEL_SPEC_DIR=docs/spec/src \
	GRUEL_SPEC_CASES=crates/gruel-spec/cases \
	$(TIMEOUT) cargo run -p gruel-spec -- --traceability
	GRUEL_BINARY=target/debug/gruel \
	GRUEL_UI_CASES=crates/gruel-ui-tests/cases \
	$(TIMEOUT) cargo run -p gruel-test-runner --bin gruel-ui-tests -- --quiet $(ARGS)
	GRUEL_BINARY=target/debug/gruel \
	GRUEL_EXAMPLES_DIR=examples \
	$(TIMEOUT) cargo run -p gruel-test-runner --bin gruel-examples-tests -- --quiet $(ARGS)

# ADR-0090 Part 5: assert that the canonical chumsky parser and the
# tree-sitter grammar agree on acceptance for every TOML case in
# crates/gruel-spec/cases/ and crates/gruel-ui-tests/cases/. Runs from
# the standalone tree-sitter-gruel/bindings/rust crate so the workspace
# build is unaffected.
tree-sitter-differential:
	$(TIMEOUT) cargo test --manifest-path tree-sitter-gruel/bindings/rust/Cargo.toml \
		--test spec_corpus_differential -- --nocapture

# ADR-0091: assert that gruel-lsp emits the same diagnostics as the
# `gruel check` code path for every TOML case in crates/gruel-spec/cases/
# and crates/gruel-ui-tests/cases/. Catches LSP/CLI drift that the
# unit-test suite would miss (e.g. a sema refactor that the LSP caller
# doesn't keep up with).
lsp-diagnostic-differential:
	$(TIMEOUT) cargo test -p gruel-lsp \
		--test spec_corpus_diagnostic_differential \
		-- --nocapture --ignored

# Reinstall the `gruel` binary that the Zed/Helix/Neovim LSP extensions
# launch. The Zed extension just runs `gruel lsp` from PATH, so this is
# how you pick up LSP changes — `cargo build` alone leaves the stale
# binary at ~/.cargo/bin/gruel in place. Restart the LSP in your editor
# (e.g. `zed: restart language server`) after this finishes.
install-lsp:
	cargo install --path crates/gruel --force

# Format all Rust files.
fmt:
	cargo fmt --all

# Check formatting without making changes (for CI).
check: check-intrinsic-docs check-builtins-docs check-spec-builtins check-stdlib-docs
	cargo check --workspace --all-targets --exclude gruel-runtime
	cargo check --manifest-path fuzz/Cargo.toml --all-targets
	cargo clippy --workspace --all-targets --exclude gruel-runtime
	cargo fmt --all -- --check

# Fail if the human-written spec mentions an `@<name>` token that is not a
# real intrinsic, a recognized directive, or in the per-file allowlist for
# documented retired/typo names. Catches prose drift that traceability and
# spec tests do not see (test bodies and paragraph IDs may be in sync while
# the surrounding text references a removed builtin).
check-spec-builtins:
	@cargo run -q -p gruel-intrinsics --bin gruel-check-spec

# Regenerate docs/generated/intrinsics-reference.md from the gruel-intrinsics
# registry. Run this after editing IntrinsicDef entries.
gen-intrinsic-docs:
	cargo run -q -p gruel-intrinsics --bin gruel-intrinsics-docs

# Fail if the checked-in intrinsics reference has drifted from what the
# registry would generate. Prevents the spec and the compiler from disagreeing.
check-intrinsic-docs:
	@mkdir -p target
	@cargo run -q -p gruel-intrinsics --bin gruel-intrinsics-docs -- \
		target/intrinsics-reference.md.generated
	@if ! diff -u docs/generated/intrinsics-reference.md \
		target/intrinsics-reference.md.generated >/dev/null; then \
		echo "docs/generated/intrinsics-reference.md is out of date."; \
		echo "Run 'make gen-intrinsic-docs' and commit the result."; \
		diff -u docs/generated/intrinsics-reference.md \
			target/intrinsics-reference.md.generated || true; \
		exit 1; \
	fi

# Regenerate docs/generated/builtins-reference.md from the gruel-builtins
# registries. Run this after editing BuiltinTypeDef / BuiltinEnumDef /
# BuiltinTypeConstructor entries.
gen-builtins-docs:
	cargo run -q -p gruel-builtins --bin gruel-builtins-docs

# Fail if the checked-in built-in types reference has drifted from what the
# registry would generate.
check-builtins-docs:
	@mkdir -p target
	@cargo run -q -p gruel-builtins --bin gruel-builtins-docs -- \
		target/builtins-reference.md.generated
	@if ! diff -u docs/generated/builtins-reference.md \
		target/builtins-reference.md.generated >/dev/null; then \
		echo "docs/generated/builtins-reference.md is out of date."; \
		echo "Run 'make gen-builtins-docs' and commit the result."; \
		diff -u docs/generated/builtins-reference.md \
			target/builtins-reference.md.generated || true; \
		exit 1; \
	fi

# ADR-0089 Phase 6: regenerate docs/generated/stdlib/ and prelude/
# trees from std/*.gruel / prelude/*.gruel. Mirrors the
# gen-intrinsic-docs / check-intrinsic-docs pattern.
gen-stdlib-docs:
	./scripts/gen-stdlib-docs.sh

check-stdlib-docs:
	@mkdir -p target
	@./scripts/gen-stdlib-docs.sh target/stdlib-docs-check > /dev/null
	@for sub in stdlib prelude; do \
		if ! diff -r docs/generated/$$sub target/stdlib-docs-check/$$sub >/dev/null; then \
			echo "docs/generated/$$sub is out of date."; \
			echo "Run 'make gen-stdlib-docs' and commit the result."; \
			diff -r docs/generated/$$sub target/stdlib-docs-check/$$sub || true; \
			exit 1; \
		fi; \
	done

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

fuzz-parser-differential:
	cargo +nightly fuzz run parser_differential -- -max_total_time=$(FUZZ_TIME)

# ADR-0090: regenerate the tree-sitter grammar's parser.c after editing
# grammar.js. Uses the tree-sitter CLI from tree-sitter-gruel/node_modules
# (installed by `npm install` inside that directory). Idempotent — produces
# the same parser.c we already commit, so contributors who only consume the
# grammar don't need node installed.
tree-sitter-generate:
	cd tree-sitter-gruel && HOME=$$(pwd) ./node_modules/.bin/tree-sitter generate

# Run tree-sitter's native corpus tests against the generated parser.
tree-sitter-test:
	cd tree-sitter-gruel && HOME=$$(pwd) ./node_modules/.bin/tree-sitter test

# Run Claude in a sandboxed environment.
claude:
	vibe --send "ln -fs ~/.claude/dot_claude_dot_json_should_have_been_here.json ~/.claude.json" \
	     --send "IS_SANDBOX=1 claude --allow-dangerously-skip-permissions --dangerously-skip-permissions --append-system-prompt='You are running in a sandboxed debian environment, so feel free to install and remove packages as needed, etc. DO NOT destructively rewrite git history or do something crazy like sending an email.'"

# Generate rustdoc for workspace library crates and stage the output under
# website/static/crate-docs/ so zola copies it into the final site.
# gruel-runtime is no_std/staticlib-only; the other excluded crates are
# binaries/test harnesses with no useful library docs.
crate-docs:
	cargo doc --workspace --no-deps \
	    --exclude gruel \
	    --exclude gruel-runtime \
	    --exclude gruel-spec \
	    --exclude gruel-test-runner
	rm -rf website/static/crate-docs
	mkdir -p website/static/crate-docs
	cp -R target/doc/. website/static/crate-docs/

# Build the website.
website: crate-docs
	./website/build.sh

# Run the website dev server at http://127.0.0.1:1111.
website-serve: crate-docs
	./website/build.sh serve

# Build the website for production deployment.
website-deploy: crate-docs
	./website/build.sh deploy
