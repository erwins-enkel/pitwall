install:
	cargo build --release
	install -Dm755 target/release/pitwall $(HOME)/.local/bin/pitwall

build:
	cargo build --release

test:
	cargo test

.PHONY: install build test
