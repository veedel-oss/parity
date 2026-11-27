# The checks and builds of CI (.github/workflows/ci.yml) with the same
# commands; change both together. `make help` lists the targets.

SHELL := bash
.SHELLFLAGS := -euo pipefail -c
.DEFAULT_GOAL := help

TARGET := x86_64-unknown-linux-musl
BIN := target/$(TARGET)/release/parity
OS := debian-12
SOURCE_DATE_EPOCH ?= $(shell git log -1 --format=%ct)

# $(call each,PATTERN) COMMAND: COMMAND on the files matching PATTERN, also
# uncommitted ones. Only *.py files go to ruff, as in CI: without paths,
# ruff format also formats code blocks in Markdown.
each = git ls-files -z --cached --others --exclude-standard '$(1)' | xargs -0 -r

DOCKERFILES := :(glob)**/Dockerfile

.PHONY: help fmt lint lint-rust lint-shell lint-python lint-docker lint-deps \
	build test static build-deb e2e check clean

help: ## List the targets
	@awk 'BEGIN { FS = ":.*## " } /^[a-z0-9-]+:.*## / { printf "  %-12s %s\n", $$1, $$2 }' \
		$(MAKEFILE_LIST)

fmt: ## Format the Rust and Python code
	cargo fmt --all
	$(call each,*.py) ruff format

lint: lint-rust lint-shell lint-python lint-docker lint-deps ## Run all lint checks

lint-rust: ## Check the Rust formatting, run clippy
	cargo fmt --all --check
	cargo clippy --workspace --all-targets --locked -- -D warnings

lint-shell: ## Run shellcheck on every *.sh file
	$(call each,*.sh) shellcheck

lint-python: ## Run ruff check and the ruff format check on every *.py file
	$(call each,*.py) ruff check
	$(call each,*.py) ruff format --check

lint-docker: ## Run hadolint on every Dockerfile
	$(call each,$(DOCKERFILES)) hadolint

lint-deps: ## Check the crate boundaries and packaging/deb/copyright
	@for pair in 'parity-collect parity-analyze' 'parity-analyze parity-collect'; do \
		read -r crate other <<<"$$pair"; \
		deps=$$(cargo tree --locked -p "$$crate" -e normal,build,dev --prefix none); \
		if grep -q "^$$other " <<<"$$deps"; then \
			echo "error: $$crate must not depend on $$other" >&2; \
			exit 1; \
		fi; \
	done
	@linked=$$(cargo tree --locked -p parity --target $(TARGET) \
			-e normal,no-proc-macro --prefix none --format '{p}' | \
		grep -v ' (/' | awk '{ print $$1 }' | sort -u); \
	listed=$$(sed -n 's|^Files: vendor/\([^/]*\)/\*$$|\1|p' packaging/deb/copyright | sort -u); \
	if [[ "$$linked" != "$$listed" ]]; then \
		echo "error: packaging/deb/copyright does not list the linked crates (< linked, > listed)" >&2; \
		diff <(echo "$$linked") <(echo "$$listed"); \
		exit 1; \
	fi

build: ## Build all targets of the workspace (development build)
	cargo build --workspace --all-targets --locked

test: ## Run the workspace tests
	cargo test --workspace --locked

static: ## Build the static release binary and check it as CI does
	cargo build --release --locked --target $(TARGET)
	@info=$$(file $(BIN)); \
	echo "$$info"; \
	if ! grep -q 'statically linked\|static-pie linked' <<<"$$info"; then \
		echo "error: $(BIN) is not statically linked" >&2; \
		exit 1; \
	fi
	@size=$$(stat -c %s $(BIN)); \
	echo "size: $$size bytes"; \
	if (( size >= 5000000 )); then \
		echo "error: $(BIN) is not smaller than the limit of 5000000 bytes" >&2; \
		exit 1; \
	fi
	$(BIN) --version

# Only build.sh runs in the container. It has no git, so the commit time is
# passed in.
build-deb: static ## Build the Debian package in target/, with dpkg-deb from docker
	docker build -q -t parity-deb packaging/deb >/dev/null
	docker run --rm --network none --user "$$(id -u):$$(id -g)" \
		-v "$(CURDIR):/src:ro" -v "$(CURDIR)/target:/src/target" -w /src \
		-e SOURCE_DATE_EPOCH=$(SOURCE_DATE_EPOCH) \
		parity-deb packaging/deb/build.sh $(BIN) target

e2e: ## Run the e2e tests on VMs with OS (make e2e OS=debian-10)
	tests/e2e/run.sh $(OS)

# Prerequisites inherit the variable.
check: export CARGO_BUILD_WARNINGS := deny
check: lint build test static ## Run everything ci.yml runs

clean: ## Remove the build output
	cargo clean
