# Implementation Status

Last audited: 2026-05-03  
Repository: `github.com/flotob/freedom-ipfs`  
Spec: `/root/codex/mobile-rust-ipfs-node-spec.md`

Detailed prompt-to-artifact audit: `docs/completion-audit.md`

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

Kubo-generated UnixFS, UnixFS range, and HAMT-directory parity smoke passed with Kubo v0.41.0 downloaded locally to `target/tools/kubo/kubo/ipfs`:

```bash
KUBO_BIN=$PWD/target/tools/kubo/kubo/ipfs cargo test -p freedom-ipfs-gateway --test kubo_parity -- --ignored --nocapture
```

Live public-network smoke passed:

```bash
make live-smoke
```

Public CID corpus smoke passed for the checked-in immutable paths:

```bash
make live-corpus
```

Local cached-gateway soak passed:

```bash
make local-soak
```

Observed live-smoke result:

- `vitalik.eth` resolved at runtime to `/ipfs/bafybeiaql2jo3fu5b7c4lmpoi5drh5sam7yt652shwdgwbky4o7uw33u2u`
- `daicowtf.eth` resolved at runtime to `/ipfs/bafybeidznfolm74c5cephzdycedx7hk76iawno45wemcvkflieotzo2lne`
- local gateway returned `38394` bytes for `vitalik.eth`
- local gateway returned `403507` bytes for `daicowtf.eth`
- retrieval stats were `cache_hits=25 http_provider_blocks=2 bitswap_blocks=3`

Observed live-corpus result:

- `vitalik-home` fetched `/ipfs/bafybeiaql2jo3fu5b7c4lmpoi5drh5sam7yt652shwdgwbky4o7uw33u2u`
- `daicowtf-home` fetched `/ipfs/bafybeidznfolm74c5cephzdycedx7hk76iawno45wemcvkflieotzo2lne`
- retrieval stats were `cache_hits=25 http_provider_blocks=2 bitswap_blocks=3`

Observed local-soak result:

- 500 cached local-gateway requests completed against an in-memory raw block.
- Linux RSS moved from `9216` KiB to `13696` KiB, within the 32 MiB maximum growth budget.

iOS packaging command was exercised on Linux and correctly refused to run:

```bash
cargo run -p xtask -- build-xcframework
cargo run -p xtask -- verify-xcframework
```

Result:

```text
Error: build-xcframework requires macOS with Xcode command line tools; current host is linux
Error: verify-xcframework requires macOS with Xcode command line tools; current host is linux
```

## Roadmap Audit

M0 decisions and fixtures: partially complete. The repo, license, generated unit-test fixtures, deterministic libp2p fixtures, and Kubo-generated UnixFS/HAMT CAR parity smokes exist. A larger checked-in public fixture corpus is still useful.

M1 workspace and mobile skeleton: mostly complete. Workspace, mobile C ABI, Swift wrapper source, gateway start/stop, stats, cache import/export, routing mode selection, multi-router configuration, local gateway URL mapping helpers, lifecycle hooks, preload/cancel with path/URI/bare-CID normalization, and an XCFramework build/verify skeleton exist. The build stages the C header plus module map; the verifier checks exported C symbols and attempts a simulator Swift link smoke on macOS. macOS/Xcode artifact production and simulator link/start remain unverified here.

M2 CID, block verification, and store: complete for MVP. CID parse/format, verified block insertion, CAR import/export, bounded in-memory hot block cache, SQLite cache, eviction, provider cache, bad-provider cache, clear, and trim are covered by tests.

M3 UnixFS reader and offline gateway: complete for MVP. Raw, dag-pb, multi-block files, directories, directory `index.html` fallback with path-based MIME headers, basic HAMT traversal, range reads, malformed/unsatisfiable byte-range rejection, streaming gateway responses, traversal-segment rejection, and Kubo-generated UnixFS/HAMT CAR import/gateway byte parity are implemented and tested.

M4 IPNS and DNSLink: implemented. DNSLink uses a pluggable TXT resolver trait and a generic default resolver wrapper, with Cloudflare DoH as the current shipped backend. DNS TXT TTLs are preserved when available and capped by the name cache. IPNS delegated lookup, light-DHT fallback, v2 verification, expiry checks, recursion-limit failure, and name caching are implemented and tested. Native/system TXT lookup remains a follow-up.

M5 delegated routing and verified HTTP retrieval: implemented. Delegated Routing V1 parsing, CIDv1/base32 lookup normalization, optional comma-separated multi-router race/failover for provider discovery, malformed/oversized routing response rejection, bounded delegated response size/provider fanout, provider caching, bounded HTTP raw block retrieval, CID verification, invalid/redirected/oversized provider block rejection, bad-provider suppression, and HTTP timeouts are implemented.

M6 minimal Bitswap client: implemented for read-only retrieval. It dials bounded provider candidates, supports TCP/WebSocket/QUIC transports, includes libp2p identify/ping behaviours, applies libp2p connection timeout/connection-limit guards, verifies returned blocks, caches extra payload blocks, and sends cancels. It does not serve blocks. The retrieval crate includes a deterministic in-process libp2p Bitswap peer test that validates stream negotiation, block response handling, cache insertion, and cancel emission.

M7 light DHT fallback: implemented for provider lookup and IPNS record lookup. It uses Kademlia client mode, lazy per-lookup swarms, query timeout, provider fanout limits, libp2p identify/ping behaviours, and libp2p connection timeout/connection-limit guards. The routing crate includes deterministic local server-mode Kademlia peer tests for provider lookup and verified IPNS record lookup through the light-DHT client.

M8 mobile resource hardening: partially complete. Bounded in-memory hot block cache, cache trim, gateway concurrency limit, mobile background/foreground hooks, low-memory trim hook, network-change provider-cache hygiene, DHT timeout/fanout knobs, provider/badness caches, bounded HTTP provider response bodies, HTTP timeouts, libp2p identify/ping behaviours, libp2p connection timeouts, and libp2p connection-limit guards exist. Real idle RSS, CPU, network, startup, Bee concurrency, and host-app lifecycle behavior are not measured yet.

M9 browser integration: partially complete. The local gateway path, mobile ABI, Swift wrapper source, gateway URL mapping helpers for `ipfs://`, `ipns://`, `/ipfs`, and `/ipns` addresses, preload normalization for path/URI/bare-CID inputs, lifecycle hooks, and preload/cancel controls exist, and the live smoke proves ENS-backed contenthash flows when names are resolved outside the node. Swift wrapper compilation/linking and app integration are not verified in this Linux environment.

M10 interop hardening: partial. Unit tests, deterministic local Bitswap and light-DHT coverage, Kubo-generated UnixFS/HAMT/range parity smoke, live ENS smoke, a small checked-in public CID corpus smoke, and a local cached-gateway RSS soak exist, but a larger public CID corpus, broader Kubo parity matrix, and longer network/device soak tests remain follow-up work.

M11 optional features: not started except CAR export/import support, which was promoted into the MVP diagnostics/cache path.

## Known Gaps

- Real iOS XCFramework creation and symbol/Swift-link verification require macOS with Xcode; `xtask verify-xcframework` exists but cannot run on this Linux host.
- Swift wrapper source exists, but `swift` is not installed in this Linux environment, and sample app link/start/stop tests require macOS/Xcode.
- Real iPhone resource targets are unverified, including the provisional under-60-MiB idle RSS target beside Bee.
- iOS lifecycle hooks exist at the ABI/Swift level, but actual host-app background/foreground, low-memory, and network-path event wiring is not verified on iOS.
- DHT-only retrieval of `daicowtf.eth` is not reliable on the public DHT; current auto mode succeeds because delegated routing returns usable providers.
- DNSLink still defaults to Cloudflare DoH, with TTL-aware caching. Native/system TXT lookup should be evaluated for artifact size and iOS behavior.
- The checked-in public CID corpus is intentionally small; it needs more IPNS, DNSLink, large/range-media, and documented pass/fail cases.
- The local soak is host-side only; long network soaks and iOS device memory-growth soaks are still missing.
- No broad Kubo parity matrix exists yet for deterministic cross-implementation gateway/routing regression coverage.
