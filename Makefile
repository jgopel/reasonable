.PHONY: all
all: test quality

.PHONY: test
test:
	cargo test --all-targets --all-features

.PHONY: quality
quality:
	poetry run pre-commit run --all-files
