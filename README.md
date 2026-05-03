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
- shared delegated endpoint configuration for provider routing and delegated IPNS lookup,
- short-lived in-memory DNSLink/IPNS name-resolution cache,
- delegated routing provider lookup,
- client-mode light DHT provider lookup fallback,
- short-lived multihash-keyed provider-result cache and bad-provider suppression,
- verified HTTP raw-block retrieval from HTTP-capable providers,
- client-only Bitswap retrieval over TCP, WebSocket, and QUIC libp2p streams for Bitswap-only providers,
- bounded Bitswap peer/address fanout with one provider-refresh retry after stale-provider failures,
- Bitswap cancel messages after successful block receipt,
- verified caching of extra CIDv0/CIDv1 blocks returned in Bitswap payload responses,
- basic HAMT-sharded UnixFS directory traversal,
- UnixFS range reads that avoid assembling entire multi-block files for byte-range responses,
- full local-gateway responses streamed in bounded UnixFS chunks instead of one full-file buffer,
- configurable local-gateway request concurrency limiting,
- CAR import/export for tests, diagnostics, and cache warmup,
- an iOS staticlib/XCFramework build skeleton with C ABI headers for persistent-cache node creation, cache trimming, routing-mode selection, and offline/online gateway start.

Still incomplete: production iOS packaging validation, resource profiling on device, and hardened DHT-only retrieval for sites whose DHT providers are slow or stale.

## Development

```bash
cargo test --workspace
cargo run -p freedom-ipfs-gateway -- --help
```

Run the gateway online with the mobile default `auto` routing mode:

```bash
cargo run -p freedom-ipfs-gateway -- --online --routing-mode auto
```

Limit local-gateway request concurrency for mobile resource testing:

```bash
cargo run -p freedom-ipfs-gateway -- --online --max-concurrent-requests 4
```

On macOS with Xcode command line tools installed, build the iOS static libraries and XCFramework:

```bash
cargo run -p xtask -- build-xcframework
```

Live smoke test, intentionally ignored by default because it uses the public IPFS network:

```bash
FREEDOM_IPFS_LIVE_ENS=vitalik.eth,daicowtf.eth \
  cargo test -p freedom-ipfs-gateway --test live_smoke -- --ignored --nocapture
```

Light-DHT provider discovery smoke:

```bash
cargo test -p freedom-ipfs-routing live_light_dht_finds_public_providers -- --ignored --nocapture
```

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
