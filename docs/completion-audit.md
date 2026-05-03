# Completion Audit

Last audited: 2026-05-03  
Repository: `github.com/flotob/freedom-ipfs`  
Implementation audited head: the commit containing this file version  
Spec: `/root/codex/mobile-rust-ipfs-node-spec.md`

## Verdict

The Linux-verifiable MVP implementation is substantially present and running, but the full thread goal is not complete yet.

The remaining required evidence depends on macOS/Xcode and real iPhone validation:

- Produce and verify the real iOS `FreedomIpfs.xcframework`.
- Compile/link the Swift wrapper or a sample app against the XCFramework.
- Start/stop the library on an iOS simulator.
- Measure startup, idle RSS, CPU, network idleness, active retrieval, background/foreground, low-memory, network-change, and Bee co-residency behavior on target iPhones.

Do not mark the overall implementation goal complete until those Apple-platform gates pass.

## Current Verification Evidence

Fresh checks run at `50d40d1`:

```bash
cargo fmt --all --check && make verify
make live-smoke && make live-corpus
KUBO_BIN=$PWD/target/tools/kubo/kubo/ipfs make kubo-parity
make local-soak
cargo run -p xtask -- build-xcframework
cargo run -p xtask -- verify-xcframework
```

Results:

- `cargo fmt --all --check && make verify` passed.
- Live smoke passed through the local gateway:
  - `vitalik.eth` resolved to `/ipfs/bafybeiaql2jo3fu5b7c4lmpoi5drh5sam7yt652shwdgwbky4o7uw33u2u`, returned `38394` bytes.
  - `daicowtf.eth` resolved to `/ipfs/bafybeidznfolm74c5cephzdycedx7hk76iawno45wemcvkflieotzo2lne`, returned `403507` bytes.
  - Retrieval stats: `cache_hits=25 http_provider_blocks=2 bitswap_blocks=3`.
- Public corpus smoke passed for the checked-in `vitalik-home` and `daicowtf-home` entries with the same byte counts and retrieval stats.
- Kubo parity passed for generated UnixFS site files/directories and HAMT directories.
- Local cached-gateway soak passed: `500` requests, Linux RSS from `8704` KiB to `13696` KiB.
- `xtask build-xcframework` and `xtask verify-xcframework` correctly refused to run on Linux with the macOS/Xcode requirement message.

## Prompt-To-Artifact Checklist

| Requirement | Evidence | Status |
|---|---|---|
| Create `github.com/flotob/freedom-ipfs` | Git remote is `https://github.com/flotob/freedom-ipfs`; commits are pushed to `main`. | Done |
| Dual MIT/Apache-2.0 licensing | `LICENSE-MIT`, `LICENSE-APACHE`, workspace license `MIT OR Apache-2.0`. | Done |
| Rust workspace from scratch | Workspace crates exist for core, store, unixfs, namesys, routing, retrieval, gateway, mobile, and `xtask`. | Done |
| Clean-room implementation discipline | No vendored IPFS implementation is present; existing projects are listed as mining targets in the spec. | Done |
| CIDv0/CIDv1 parse/format | `freedom-ipfs-core` unit tests cover CID round-trip and verification. | Done |
| Block verification before cache/serve | Core verification, store verified insertion, retrieval invalid HTTP block rejection. | Done |
| Bounded SQLite cache | `freedom-ipfs-store` implements SQLite cache, LRU eviction, stats, clear, trim, provider and bad-provider caches. | Done |
| Memory hot cache | Store has a bounded 16 MiB in-memory hot block cache in front of SQLite; tests cover clear/trim removing hot entries. | Done |
| Default disk cache 256 MiB | CLI/mobile default to `256 * 1024 * 1024`. | Done |
| CAR import/export for fixtures/cache warmup | Core CAR parse/encode and store import/export tests; CLI/mobile import/export APIs. | Done |
| Max block guard | Core verifies blocks with a max block size path. | Done for MVP |
| UnixFS raw/dag-pb files | `freedom-ipfs-unixfs` tests cover raw and dag-pb files. | Done |
| Multi-block UnixFS raw leaves | Range-across-inline-and-linked-block tests plus Kubo parity. | Done |
| Directories and path resolution | UnixFS directory tests and gateway directory index test. | Done |
| HAMT directories | UnixFS HAMT tests and Kubo-generated HAMT parity. | Done for basic real-world compatibility |
| Local HTTP gateway data plane | Gateway crate and binary serve `/health`, `/ipfs`, `/ipns`; live smoke uses the local gateway. | Done |
| Streaming responses | Gateway streams full responses in bounded chunks. | Done |
| HTTP range support | Gateway range tests cover success, malformed ranges, and unsatisfiable ranges. | Done |
| Browser MIME behavior | Directory `index.html` fallback with path-based `text/html` test. | Done for MVP |
| Useful gateway status codes | Tests cover invalid path/range, not found/name not found, timeout, busy, and traversal cases. | Mostly done |
| Bounded gateway concurrency | Gateway semaphore and concurrency-limit test. | Done |
| IPNS support in first product-usable release | Delegated IPNS, light-DHT IPNS fallback, v2 verification, expiry, tamper, recursion-limit tests. | Done |
| DNSLink support | DNSLink parser, pluggable TXT resolver trait, generic default resolver wrapper, Cloudflare DoH backend, tests. | Done |
| Native/system TXT resolver | Explicit follow-up in spec/status. | Not required for MVP; open |
| ENS excluded from node | Live smoke resolves ENS/contenthash outside the node, then feeds `/ipfs` paths to the local gateway. | Done |
| Delegated Routing V1 | Routing parser/client, JSON/NDJSON tests, response byte cap, provider fanout cap. | Done |
| Multiple delegated routers | `DelegatedRoutingClient::with_endpoints`, comma-separated CLI/mobile provider-routing config, race/failover test. | Done |
| Default delegated router | `https://delegated-ipfs.dev/routing/v1`. | Done |
| Verified HTTP raw-block retrieval | Retrieval requests provider `/ipfs/{cid}?format=raw`, disables redirects, verifies CIDs. | Done |
| Invalid provider blocks fail closed | Invalid and redirected HTTP provider tests; bad-provider suppression. | Done |
| No public trustless gateway fallback | No public gateway fallback path exists; live smoke goes through routing/providers and local gateway. | Done |
| Minimal Bitswap client | Deterministic in-process libp2p Bitswap peer test verifies retrieval, cache insert, and cancel behavior. | Done for MVP |
| Bitswap transports | TCP/WebSocket/QUIC configured; supported address filtering tests. | Done |
| No block serving to peers | Retrieval implements client-only Bitswap path and does not expose a serving strategy. | Done by design, needs packet-level/device audit for final confidence |
| Light DHT fallback | Kademlia client mode provider lookup and IPNS record lookup, deterministic local DHT tests. | Done |
| Lazy/idle network behavior | DHT swarms are per lookup; Bitswap sessions are bounded; no background maintenance loops are apparent. | Partially verified; device/network inspection still needed |
| Routing modes | `auto`, `delegated`, `light_dht`, `offline` paths exist across CLI/mobile/gateway constructors. | Done |
| iOS-first C ABI | `freedom-ipfs-mobile`, `ffi/include/freedom_ipfs.h`, and lifecycle/cache/gateway/preload APIs exist. | Done |
| Swift wrapper | `ffi/swift/FreedomIpfsReader.swift` exists with gateway start/stop, stats, cache, lifecycle, preload/cancel, multi-router, and local URL mapping helpers. | Source present; not compiled here |
| XCFramework build skeleton | `xtask build-xcframework` builds iOS targets and packages headers/module map on macOS. | Skeleton done; not produced on Linux |
| XCFramework verifier | `xtask verify-xcframework` checks slices, headers, module maps, exported symbols on macOS. | Skeleton done; not run on macOS |
| Simulator smoke link/start | Required by spec. | Missing, needs macOS/Xcode |
| Real iPhone resource target under 60 MiB RSS beside Bee | Required by spec. | Missing, needs device |
| Lifecycle hooks | ABI/Swift hooks for background, foreground, low memory, network change; unit tests cover behavior. | Implemented; host-app/device wiring unverified |
| Browser integration helpers | Local gateway URL, `ipfs://`/`ipns://`/gateway-style URL mapping helpers, preload/cancel with path/URI/bare-CID normalization, cache stats/control. | Source present; Swift compile/link unverified |
| Live ENS-backed smoke | `make live-smoke` resolves `vitalik.eth` and `daicowtf.eth` at runtime and fetches through local gateway. | Done on Linux |
| Public CID corpus | Small checked-in corpus and opt-in smoke. | Started; should grow |
| Kubo parity | Deterministic Kubo-generated UnixFS/HAMT parity tests. | Started; should grow |
| Long-running soak | Local cached-gateway RSS soak exists and passes. | Host-side only; device/network soak missing |
| Security parser/network limits | Tests cover oversized/malformed routing, traversal, invalid blocks, redirects, IPNS tamper/expiry, recursion, provider fanout. | Good MVP coverage |

## Remaining Work To Close The Goal

1. Run `make build-xcframework` on macOS with Xcode command line tools and fix any Rust dependency or target-link issues.
2. Run `make verify-xcframework` on macOS and extend it to compile/link a minimal Swift sample if needed.
3. Add or run an iOS simulator smoke that imports an offline CAR fixture, starts the gateway, and renders/fetches through loopback.
4. Integrate the Swift wrapper into the Freedom iOS app and wire background/foreground, low-memory, and network-path events.
5. Profile real devices with Bee running beside this node and record RSS, CPU, startup, active retrieval, and idle network behavior.
6. Expand the public corpus with more stable IPFS/IPNS/DNSLink paths and larger/range-media cases.
7. Add longer host and device soaks for live retrieval, not just cached local reads.

## Completion Rule

The goal can be marked complete only after the Apple-platform gates pass and the device resource profile shows the node is usable beside Bee, or the spec is explicitly revised to remove those requirements.
