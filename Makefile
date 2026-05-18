.PHONY: build build-static build-xdp test test-live lint audit bench bench-ramp

build:
	cargo build --release

build-static:
	cargo build --release --target x86_64-unknown-linux-musl

build-xdp:
	cargo build --release --features xdp

test:
	cargo test

test-live:
	cargo test -- --ignored

lint:
	cargo clippy --all-targets -- -D warnings

audit:
	cargo audit

bench:
	./target/release/dnsmark -s 192.168.1.10 --random -l 30

bench-ramp:
	./target/release/dnsmark -s 192.168.1.10 --random --ramp
