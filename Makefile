DATA_DIR ?= hadar-data
RUST_LOG ?= info
export RUST_LOG

# Warnings are errors when building docs
DOCFLAGS := -D warnings

.DEFAULT_GOAL := help

.PHONY: help
help: ## List the available targets
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| sort \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

.PHONY: ci
ci: fmt-check check clippy test doc ## Run all CI check

.PHONY: fmt-check
fmt-check: ## Check formatting without changing anything
	cargo fmt --all -- --check

.PHONY: check
check: ## Type-check every target, including tests and benches
	cargo check --all-targets

.PHONY: clippy
clippy: ## Lint every target with warnings denied
	cargo clippy --all-targets --all-features -- -D warnings

.PHONY: test
test: ## Run the whole test suite
	cargo test --all-features --workspace

.PHONY: doc
doc: ## Build the docs, denying warnings
	RUSTDOCFLAGS="$(DOCFLAGS)" cargo doc --no-deps --all-features

.PHONY: fmt
fmt: ## Reformat the workspace in place
	cargo fmt --all

.PHONY: fix
fix: ## Apply the clippy suggestions that can be applied automatically
	cargo clippy --all-targets --all-features --fix --allow-staged -- -D warnings

# CRATE selects one package, so an edit to the log does not pay for the whole
# workspace: make test-crate CRATE=hadar
CRATE ?= hadar

.PHONY: test-crate
test-crate: ## Test one package
	cargo test --all-features -p $(CRATE)

.PHONY: clippy-crate
clippy-crate: ## Lint one package
	cargo clippy --all-targets --all-features -p $(CRATE) -- -D warnings

TEST ?=

.PHONY: test-one
test-one: ## Run the tests whose names match TEST
	cargo test --all-features --workspace -- --nocapture $(TEST)

.PHONY: run
run: ## Run the server against DATA_DIR
	cargo run -p hadar -- $(DATA_DIR)

.PHONY: run-release
run-release: ## Run the server with optimizations
	cargo run --release -p hadar -- $(DATA_DIR)

.PHONY: doc-open
doc-open: ## Build the docs and open them in a browser
	RUSTDOCFLAGS="$(DOCFLAGS)" cargo doc --no-deps --all-features --open

.PHONY: deny
deny: ## Audit licenses, advisories and bans (needs cargo-deny)
	cargo deny check

.PHONY: outdated
outdated: ## Report dependencies with newer releases (needs cargo-outdated)
	cargo outdated --workspace --root-deps-only

.PHONY: clean
clean: ## Remove build artifacts
	cargo clean

.PHONY: clean-data
clean-data: ## Delete DATA_DIR, discarding the store and the log
	@printf 'This deletes %s, including the store and every log segment.\n' '$(DATA_DIR)'
	@printf 'Press enter to continue, or ctrl-c to stop. '
	@read _confirm
	rm -rf -- '$(DATA_DIR)'
