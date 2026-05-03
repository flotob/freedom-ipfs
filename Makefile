LIVE_ENS ?= vitalik.eth,daicowtf.eth
KUBO_BIN ?= target/tools/kubo/kubo/ipfs

.PHONY: test fmt clippy verify live-smoke kubo-parity build-xcframework clean

test:
	cargo test --workspace

fmt:
	cargo fmt --all

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

verify: test clippy

live-smoke:
	FREEDOM_IPFS_LIVE_ENS="$(LIVE_ENS)" cargo test -p freedom-ipfs-gateway --test live_smoke -- --ignored --nocapture

kubo-parity:
	KUBO_BIN="$(KUBO_BIN)" cargo test -p freedom-ipfs-gateway --test kubo_parity -- --ignored --nocapture

build-xcframework:
	cargo run -p xtask -- build-xcframework

clean:
	cargo clean
