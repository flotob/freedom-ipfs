.PHONY: test fmt clippy build-xcframework clean

test:
	cargo test --workspace

fmt:
	cargo fmt --all

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

build-xcframework:
	cargo run -p xtask -- build-xcframework

clean:
	cargo clean
