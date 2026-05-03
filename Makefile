LIVE_ENS ?= vitalik.eth,daicowtf.eth
KUBO_BIN ?= target/tools/kubo/kubo/ipfs

.PHONY: test fmt clippy verify validate-ios-device-evidence local-soak live-smoke live-corpus live-soak kubo-parity build-xcframework verify-xcframework clean

test:
	cargo test --workspace

fmt:
	cargo fmt --all

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

verify: test clippy validate-ios-device-evidence

validate-ios-device-evidence:
	cargo run -p xtask -- validate-ios-device-evidence

local-soak:
	cargo test -p freedom-ipfs-gateway --test local_soak -- --ignored --nocapture

live-smoke:
	FREEDOM_IPFS_LIVE_ENS="$(LIVE_ENS)" cargo test -p freedom-ipfs-gateway --test live_smoke -- --ignored --nocapture

live-corpus:
	cargo test -p freedom-ipfs-gateway --test public_corpus -- --ignored --nocapture

live-soak:
	cargo test -p freedom-ipfs-gateway --test live_soak -- --ignored --nocapture

kubo-parity:
	KUBO_BIN="$(KUBO_BIN)" cargo test -p freedom-ipfs-gateway --test kubo_parity -- --ignored --nocapture

build-xcframework:
	cargo run -p xtask -- build-xcframework

verify-xcframework:
	cargo run -p xtask -- verify-xcframework

clean:
	cargo clean
