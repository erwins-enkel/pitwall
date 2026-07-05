# Activate the versioned git hooks in .githooks/ for this clone. core.hooksPath
# is local, unversioned config, so a fresh checkout must run this once to get the
# pre-commit / pre-push / commit-msg guardrails.
hooks:
	git config core.hooksPath .githooks
	@echo "git hooks activated (core.hooksPath -> .githooks)"

# Install the extra tools the hooks/CI gate call. cog lints commit messages;
# cargo-machete audits unused deps. Local hooks fail open without them, so a
# fresh clone needs this once to make the local gate mirror CI.
tools:
	cargo install cocogitto --version 7.0.0
	cargo install cargo-machete

# One-time setup for a fresh clone: gate tools + git hooks.
setup: tools hooks

install:
	cargo build --release
	install -Dm755 target/release/pitwall $(HOME)/.local/bin/pitwall

build:
	cargo build --release

test:
	cargo test

# Regenerate docs/pitwall.png (dummy-data screenshot) from the screenshot example.
# Requires: rsvg-convert (librsvg) and JetBrains Mono Nerd Font at generation time
#   Arch: pacman -S librsvg ttf-jetbrains-mono-nerd
# The intermediate SVG goes under target/ (gitignored); only the PNG is committed.
screenshot:
	@command -v rsvg-convert >/dev/null 2>&1 || { echo "error: rsvg-convert not found — install librsvg"; exit 1; }
	@mkdir -p target docs
	cargo run --release --example screenshot > target/pitwall.svg
	rsvg-convert --zoom 2 target/pitwall.svg -o docs/pitwall.png
	@echo "wrote docs/pitwall.png"

.PHONY: hooks tools setup install build test screenshot
