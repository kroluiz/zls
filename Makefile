.PHONY: all build check clean fmt install install-user install-skill lint popup run test

PREFIX ?= /usr
BINDIR ?= $(PREFIX)/bin

all: build

build:
	cargo build --release

check:
	cargo check

test:
	cargo test

fmt:
	cargo fmt

lint:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

run:
	cargo run --release -- ui

popup: build
	./target/release/zls popup

install: build
	install -Dm755 target/release/zls "$(DESTDIR)$(BINDIR)/zls"

install-user:
	cargo install --path .

install-skill:
	install -Dm644 skills/zls/SKILL.md "$(HOME)/.config/opencode/skills/zls/SKILL.md"

clean:
	cargo clean
