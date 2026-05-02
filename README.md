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

- cache-backed local gateway for `/ipfs` and DNSLink-backed `/ipns`,
- delegated routing provider lookup,
- verified HTTP raw-block retrieval from HTTP-capable providers,
- client-only Bitswap retrieval over libp2p streams for Bitswap-only providers,
- CAR import for tests/cache warmup,
- an iOS staticlib/XCFramework build skeleton.

Still incomplete: full IPNS record verification, light DHT fallback, HAMT-sharded UnixFS directories, production iOS packaging validation, and resource profiling on device.

## Development

```bash
cargo test --workspace
cargo run -p freedom-ipfs-gateway -- --help
```

Live smoke test, intentionally ignored by default because it uses the public IPFS network:

```bash
FREEDOM_IPFS_LIVE_ENS=vitalik.eth,daicowtf.eth \
  cargo test -p freedom-ipfs-gateway --test live_smoke -- --ignored --nocapture
```

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
