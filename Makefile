.PHONY: all
all: test quality check-all-features udeps

.PHONY: test
test:
	cargo nextest run --all-targets --all-features

.PHONY: quality
quality:
	poetry run pre-commit run --all-files

.PHONY: check-all-features
check-all-features:
	RUSTFLAGS=-Awarnings cargo hack check --feature-powerset --all-targets

.PHONY: udeps
udeps:
	RUSTFLAGS=-Awarnings cargo +nightly hack udeps --feature-powerset --all-targets
