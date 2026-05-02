# freedom-ipfs

Mobile-first Rust IPFS reader for Freedom.

This repository implements the plan in `/root/codex/mobile-rust-ipfs-node-spec.md`:

- local HTTP gateway as the browser data plane,
- read-only IPFS/IPNS/DNSLink retrieval,
- verified blocks before serving,
- bounded cache,
- iOS-first packaging,
- no content providing, DHT server mode, public gateway fallback, or Kubo RPC compatibility in the first version.

The initial implementation is intentionally split into small crates so the offline data path can be tested before networking is added.

## Status

Early implementation. The first target is an offline/cache-backed gateway and XCFramework build skeleton, followed by delegated routing, verified HTTP retrieval, Bitswap, and light-DHT fallback.

## Development

```bash
cargo test --workspace
cargo run -p freedom-ipfs-gateway -- --help
```

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
