# Implementation Status

Last audited: 2026-05-03  
Repository: `github.com/flotob/freedom-ipfs`  
Spec: `/root/codex/mobile-rust-ipfs-node-spec.md`

## Current State

This is a running Rust IPFS reader, not just a scaffold. It starts a local gateway, resolves externally supplied `/ipfs` and `/ipns` paths, discovers providers through delegated routing with light-DHT fallback, retrieves verified blocks through HTTP providers and Bitswap, reads UnixFS data, and serves browser-facing responses from the local gateway.

The implementation remains iOS-first but has only been built and tested on Linux in this environment. Production iOS validation, simulator linking, and real-device resource profiling still require macOS/Xcode and target iPhones.

## Verification

Host verification passed:

```bash
cargo fmt --all --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Live public-network smoke passed:

```bash
make live-smoke
```

Observed live-smoke result:

- `vitalik.eth` resolved at runtime to `/ipfs/bafybeiaql2jo3fu5b7c4lmpoi5drh5sam7yt652shwdgwbky4o7uw33u2u`
- `daicowtf.eth` resolved at runtime to `/ipfs/bafybeidznfolm74c5cephzdycedx7hk76iawno45wemcvkflieotzo2lne`
- local gateway returned `38394` bytes for `vitalik.eth`
- local gateway returned `403507` bytes for `daicowtf.eth`
- retrieval stats were `cache_hits=25 http_provider_blocks=2 bitswap_blocks=3`

iOS packaging command was exercised on Linux and correctly refused to run:

```bash
cargo run -p xtask -- build-xcframework
```

Result:

```text
Error: build-xcframework requires macOS with Xcode command line tools; current host is linux
```

## Roadmap Audit

M0 decisions and fixtures: partially complete. The repo, license, and tests exist. Fixture coverage is embedded in unit tests, but a fuller reproducible Kubo fixture corpus is still useful.

M1 workspace and mobile skeleton: mostly complete. Workspace, mobile C ABI, gateway start/stop, stats, cache import/export, routing mode selection, and XCFramework build skeleton exist. macOS/Xcode artifact production and simulator link/start remain unverified here.

M2 CID, block verification, and store: complete for MVP. CID parse/format, verified block insertion, CAR import/export, SQLite cache, eviction, provider cache, bad-provider cache, clear, and trim are covered by tests.

M3 UnixFS reader and offline gateway: complete for MVP. Raw, dag-pb, multi-block files, directories, basic HAMT traversal, range reads, and streaming gateway responses are implemented and tested.

M4 IPNS and DNSLink: implemented. DNSLink uses Cloudflare DoH through a pluggable trait. IPNS delegated lookup, light-DHT fallback, v2 verification, expiry checks, and name caching are implemented and tested. Native/system TXT lookup remains a follow-up.

M5 delegated routing and verified HTTP retrieval: implemented. Delegated Routing V1 parsing, provider caching, HTTP raw block retrieval, CID verification, bad-provider suppression, and HTTP timeouts are implemented.

M6 minimal Bitswap client: implemented for read-only retrieval. It dials bounded provider candidates, supports TCP/WebSocket/QUIC transports, verifies returned blocks, caches extra payload blocks, and sends cancels. It does not serve blocks. The retrieval crate includes a deterministic in-process libp2p Bitswap peer test that validates stream negotiation, block response handling, cache insertion, and cancel emission.

M7 light DHT fallback: implemented for provider lookup and IPNS record lookup. It uses Kademlia client mode, lazy per-lookup swarms, query timeout, and provider fanout limits. The routing crate includes deterministic local server-mode Kademlia peer tests for provider lookup and verified IPNS record lookup through the light-DHT client.

M8 mobile resource hardening: partially complete. Cache trim, concurrency limit, DHT timeout/fanout knobs, provider/badness caches, and network timeouts exist. Real idle RSS, CPU, network, startup, Bee concurrency, background/foreground, and low-memory behavior are not measured yet.

M9 browser integration: partially complete. The local gateway path and mobile ABI exist, and the live smoke proves ENS-backed contenthash flows when names are resolved outside the node. Swift wrapper and app integration are not implemented in this repo.

M10 interop hardening: partial. Unit tests, deterministic local Bitswap and light-DHT coverage, and live smoke exist, but public CID corpus tests, Kubo parity matrix, and long soak tests remain follow-up work.

M11 optional features: not started except CAR export/import support, which was promoted into the MVP diagnostics/cache path.

## Known Gaps

- Real iOS XCFramework creation and symbol/link verification require macOS with Xcode.
- Swift wrapper and sample app link/start/stop test are not present.
- Real iPhone resource targets are unverified, including the provisional under-60-MiB idle RSS target beside Bee.
- DHT-only retrieval of `daicowtf.eth` is not reliable on the public DHT; current auto mode succeeds because delegated routing returns usable providers.
- DNSLink still defaults to Cloudflare DoH. Native/system TXT lookup should be evaluated for artifact size and iOS behavior.
- No controlled local Kubo parity harness exists yet for deterministic cross-implementation gateway/routing regression coverage.
