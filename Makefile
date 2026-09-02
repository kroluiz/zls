.PHONY: all build check clean fmt install lint popup run test

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

install:
	cargo install --path .

clean:
	cargo clean
