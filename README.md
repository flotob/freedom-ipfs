# freedom-ipfs

Mobile-first Rust IPFS reader for Freedom.

This repository implements the plan in `/root/codex/mobile-rust-ipfs-node-spec.md`:

- local HTTP gateway as the browser data plane,
- read-only IPFS/IPNS/DNSLink retrieval,
- verified blocks before serving,
- bounded cache,
- iOS-first packaging,
- no content providing, DHT server mode, public gateway fallback, or Kubo RPC compatibility in the first version.

The implementation is split into small crates so the local gateway, cache, routing, retrieval, namesystem, and mobile FFI surfaces can be evolved independently.

## Status

Early implementation. Current code supports:

- cache-backed local gateway for `/ipfs` and DNSLink/IPNS-backed `/ipns`,
- IPNS record retrieval and v2 signature/validity verification,
- pluggable DNSLink TXT resolver wiring, with Cloudflare DoH as the current default,
- shared delegated endpoint configuration for provider routing and delegated IPNS lookup,
- binary `/ipns/` light-DHT record lookup fallback for IPNS in `auto` and `light_dht` modes,
- deterministic in-process light-DHT IPNS record lookup test coverage,
- offline gateway constructors keep name resolution cache-only/no-network by default,
- short-lived in-memory DNSLink/IPNS name-resolution cache,
- delegated routing provider lookup, including optional comma-separated multi-router race/failover,
- bounded delegated-routing response size and provider fanout,
- client-mode light DHT provider/IPNS lookup fallback with configurable provider fanout,
- deterministic in-process light-DHT provider lookup test coverage,
- CLI/mobile knobs for DHT query timeout and provider fanout,
- short-lived multihash-keyed provider-result cache and bad-provider suppression,
- explicit HTTP timeouts for delegated routing, DNSLink/IPNS, and provider block requests,
- redirect-disabled HTTP provider block requests, matching the read-only verified retrieval model,
- explicit libp2p identify, ping, connection timeout, and connection-limit behaviours for DHT and Bitswap swarms,
- verified HTTP raw-block retrieval from HTTP-capable providers,
- client-only Bitswap retrieval over TCP, WebSocket, and QUIC libp2p streams for Bitswap-only providers,
- bounded Bitswap peer/address fanout with one provider-refresh retry after stale-provider failures,
- Bitswap cancel messages after successful block receipt,
- verified caching of extra CIDv0/CIDv1 blocks returned in Bitswap payload responses,
- deterministic in-process libp2p Bitswap retrieval test coverage,
- basic HAMT-sharded UnixFS directory traversal,
- directory `index.html` fallback with path-based MIME headers,
- UnixFS range reads that avoid assembling entire multi-block files for byte-range responses,
- gateway rejection of malformed and unsatisfiable byte-range requests,
- full local-gateway responses streamed in bounded UnixFS chunks instead of one full-file buffer,
- gateway rejection of `.`/`..` path traversal segments before UnixFS lookup,
- Kubo-generated CAR parity smoke for UnixFS files/directories and HAMT directories when `KUBO_BIN` is available,
- opt-in public CID corpus smoke that fetches documented immutable paths through the local gateway,
- opt-in local gateway soak that repeats cached reads and checks bounded RSS growth on Linux,
- configurable local-gateway request concurrency limiting,
- CAR import/export for tests, diagnostics, and cache warmup,
- mobile lifecycle hooks for background/foreground, low-memory cache trimming, and network-change provider cache hygiene,
- an iOS staticlib/XCFramework build skeleton with staged C headers/module map, exported-symbol verification, and a Swift wrapper for persistent-cache node creation, cache import/export, cache trimming, routing-mode selection, lifecycle hooks, preload/cancel, and offline/online gateway start.

Still incomplete: production iOS packaging validation, resource profiling on device, network path integration in the host app, and hardened DHT-only retrieval for sites whose DHT providers are slow or stale.

## Development

```bash
cargo test --workspace
cargo run -p freedom-ipfs-gateway -- --help
```

Run the gateway online with the mobile default `auto` routing mode:

```bash
cargo run -p freedom-ipfs-gateway -- --online --routing-mode auto
```

Use multiple delegated routers for provider discovery by passing a comma-separated endpoint list:

```bash
cargo run -p freedom-ipfs-gateway -- --online --delegated-router https://delegated-ipfs.dev/routing/v1,https://example-router.invalid/routing/v1
```

Limit local-gateway request concurrency for mobile resource testing:

```bash
cargo run -p freedom-ipfs-gateway -- --online --max-concurrent-requests 4
```

Tune light-DHT fallback budgets when testing mobile resource behavior:

```bash
cargo run -p freedom-ipfs-gateway -- --online --dht-query-timeout-secs 15 --dht-max-providers 8
```

On macOS with Xcode command line tools installed, build the iOS static libraries and XCFramework:

```bash
cargo run -p xtask -- build-xcframework
```

Verify an existing XCFramework artifact:

```bash
cargo run -p xtask -- verify-xcframework
```

Live smoke test, intentionally ignored by default because it uses the public IPFS network:

```bash
make live-smoke
```

Public CID corpus smoke, also ignored by default:

```bash
make live-corpus
```

Local cached gateway soak:

```bash
make local-soak
```

Light-DHT provider discovery smoke:

```bash
cargo test -p freedom-ipfs-routing live_light_dht_finds_public_providers -- --ignored --nocapture
```

Kubo fixture parity smoke, if a Kubo `ipfs` binary is available:

```bash
KUBO_BIN=/path/to/ipfs make kubo-parity
```

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
