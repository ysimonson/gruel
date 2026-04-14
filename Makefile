.PHONY: test quick-test fmt fmt-check check bench website website-serve website-deploy

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

# Build the website.
website:
	./website/build.sh

# Run the website dev server at http://127.0.0.1:1111.
website-serve:
	./website/build.sh serve

# Build the website for production deployment.
website-deploy:
	./website/build.sh deploy
