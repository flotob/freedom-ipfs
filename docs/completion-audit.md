# Completion Audit

Last audited: 2026-05-03  
Repository: `github.com/flotob/freedom-ipfs`  
Implementation audited head: the commit containing this file version  
Spec: `/root/codex/mobile-rust-ipfs-node-spec.md`

## Verdict

The Linux-verifiable MVP implementation is substantially present and running, and the iOS XCFramework/simulator smoke gates now pass in GitHub Actions. The full thread goal is not complete yet because real-device integration and resource validation remain.

The remaining required evidence depends on the Freedom iOS app and target iPhones:

- Measure startup, idle RSS, CPU, network idleness, active retrieval, background/foreground, low-memory, network-change, and Bee co-residency behavior on target iPhones.
- Integrate the Swift wrapper into the browser app and wire app lifecycle/network-path events.

Do not mark the overall implementation goal complete until the device/app gates pass.

## Current Verification Evidence

Fresh checks run against this file version:

```bash
cargo fmt --all --check && make verify
make live-smoke && make live-corpus
```

Apple-platform CI evidence:

```text
GitHub Actions iOS XCFramework run 25269150599 passed on 2026-05-03
Head SHA: 9b56d9a0ea89827d72202e7024b643624ae1ba91
Job URL: https://github.com/flotob/freedom-ipfs/actions/runs/25269150599/job/74088582571
```

Earlier checks for unchanged areas:

```bash
KUBO_BIN=$PWD/target/tools/kubo/kubo/ipfs make kubo-parity
make local-soak
```

Results:

- `cargo fmt --all --check && make verify` passed.
- Live smoke passed through the local gateway:
  - `vitalik.eth` resolved to `/ipfs/bafybeiaql2jo3fu5b7c4lmpoi5drh5sam7yt652shwdgwbky4o7uw33u2u`, returned `38394` bytes.
  - `daicowtf.eth` resolved to `/ipfs/bafybeidznfolm74c5cephzdycedx7hk76iawno45wemcvkflieotzo2lne`, returned `403507` bytes.
  - Retrieval stats: `cache_hits=25 http_provider_blocks=2 bitswap_blocks=3`.
- Public corpus smoke passed for the checked-in `vitalik-home` and `daicowtf-home` entries with the same byte counts and retrieval stats.
- Kubo parity passed for generated UnixFS site files, directory-index fallback, range reads, and HAMT directories.
- Local cached-gateway soak passed: `500` requests, Linux RSS from `8960` KiB to `13312` KiB.
- GitHub Actions `iOS XCFramework` passed on `macos-15` with Xcode 16.4. It built the real `FreedomIpfs.xcframework`, verified headers/module maps/exported C symbols, booted an iOS simulator, compiled and linked the Swift wrapper smoke, started the local gateway in the simulator via `simctl spawn booted`, fetched the CAR fixture through loopback, stopped the gateway, and uploaded `FreedomIpfs.xcframework` as artifact ID `6767876928` (`59993959` bytes).
- `xtask build-xcframework` and `xtask verify-xcframework` still correctly refuse to run on Linux with the macOS/Xcode requirement message.

## Prompt-To-Artifact Checklist

| Requirement | Evidence | Status |
|---|---|---|
| Create `github.com/flotob/freedom-ipfs` | Git remote is `https://github.com/flotob/freedom-ipfs`; commits are pushed to `main`. | Done |
| Dual MIT/Apache-2.0 licensing | `LICENSE-MIT`, `LICENSE-APACHE`, workspace license `MIT OR Apache-2.0`. | Done |
| Rust workspace from scratch | Workspace crates exist for core, store, unixfs, namesys, routing, retrieval, gateway, mobile, and `xtask`. | Done |
| Clean-room implementation discipline | No vendored IPFS implementation is present; existing projects are listed as mining targets in the spec. | Done |
| CIDv0/CIDv1 parse/format | `freedom-ipfs-core` unit tests cover CID round-trip and verification. | Done |
| Block verification before cache/serve | Core verification, store verified insertion, retrieval invalid HTTP block rejection. | Done |
| Bounded SQLite cache | `freedom-ipfs-store` implements SQLite cache, LRU eviction, active block retention during streaming, stats, clear, trim, provider and bad-provider caches. | Done |
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
| Stream eviction guard | Gateway stream scopes retain blocks they read; SQLite LRU eviction and explicit trim skip retained blocks until the stream scope releases them. | Done |
| HTTP range support | Gateway range tests cover success, malformed ranges, and unsatisfiable ranges. | Done |
| Browser MIME behavior | Directory `index.html` fallback with path-based `text/html` test. | Done for MVP |
| Useful gateway status codes | Tests cover invalid path/range, not found/name not found, timeout, busy, and traversal cases. | Mostly done |
| Bounded gateway concurrency | Gateway semaphore and concurrency-limit test. | Done |
| IPNS support in first product-usable release | Delegated IPNS, light-DHT IPNS fallback, v2 verification, expiry, tamper, recursion-limit tests. | Done |
| DNSLink support | DNSLink parser, pluggable TXT resolver trait, generic default resolver wrapper, Cloudflare DoH backend, TTL-aware records, tests. | Done |
| DNSLink cache TTL | DNSLink resolution preserves DNS TXT TTLs where the resolver supplies them; `CachedNameResolver` caps dynamic TTLs with a conservative max. | Done |
| Native/system TXT resolver | Explicit follow-up in spec/status. | Not required for MVP; open |
| ENS excluded from node | Live smoke resolves ENS/contenthash outside the node, then feeds `/ipfs` paths to the local gateway. | Done |
| Delegated Routing V1 | Routing parser/client, JSON/NDJSON tests, CIDv1/base32 lookup normalization, response byte cap, provider fanout cap. | Done |
| Multiple delegated routers | `DelegatedRoutingClient::with_endpoints`, comma-separated CLI/mobile provider-routing config, race/failover test. | Done |
| Default delegated router | `https://delegated-ipfs.dev/routing/v1`. | Done |
| Verified HTTP raw-block retrieval | Retrieval requests provider `/ipfs/{cid}?format=raw`, bounds response bodies, disables redirects, verifies CIDs. | Done |
| Invalid provider blocks fail closed | Invalid, redirected, and oversized HTTP provider tests; bad-provider suppression. | Done |
| No public trustless gateway fallback | No public gateway fallback path exists; live smoke goes through routing/providers and local gateway. | Done |
| Minimal Bitswap client | Deterministic in-process libp2p Bitswap tests verify retrieval, cache insert, want-have before want-block for multi-peer sessions, DONT_HAVE handling, and cancel behavior. | Done for MVP |
| Bitswap transports | TCP/WebSocket/QUIC configured; supported address filtering tests. | Done |
| No block serving to peers | Retrieval implements client-only Bitswap path and does not expose a serving strategy. | Done by design, needs packet-level/device audit for final confidence |
| Light DHT fallback | Kademlia client mode provider lookup and IPNS record lookup, deterministic local DHT tests. | Done |
| Lazy/idle network behavior | DHT swarms are per lookup; Bitswap sessions are bounded; no background maintenance loops are apparent. | Partially verified; device/network inspection still needed |
| Routing modes | `auto`, `delegated`, `light_dht`, `offline` paths exist across CLI/mobile/gateway constructors. | Done |
| iOS-first C ABI | `freedom-ipfs-mobile`, `ffi/include/freedom_ipfs.h`, and lifecycle/cache/gateway/preload APIs exist. | Done |
| Swift wrapper | `ffi/swift/FreedomIpfsReader.swift` exists with gateway start/stop, stats, cache, lifecycle, preload/cancel, multi-router, and local URL mapping helpers; GitHub Actions Swift simulator smoke compiled and linked it against the XCFramework. | Done for simulator smoke; app integration unverified |
| XCFramework build skeleton | `xtask build-xcframework` builds iOS targets, packages headers/module map, checks artifact structure/exported symbols using Rust `llvm-nm`, and produced `FreedomIpfs.xcframework` in GitHub Actions. | Done in macOS CI |
| XCFramework verifier | `xtask verify-xcframework` checks slices, headers, module maps, exported symbols, and a simulator Swift smoke on macOS; GitHub Actions run `25269150599` passed. | Done in macOS CI |
| Simulator smoke link/start | `xtask verify-xcframework` builds a Swift simulator executable that imports a generated CAR fixture, starts the local gateway, fetches the fixture through loopback, and stops the gateway using `simctl spawn booted`; GitHub Actions run `25269150599` passed. | Done in macOS CI |
| Real iPhone resource target under 60 MiB RSS beside Bee | Required by spec. | Missing, needs device |
| Lifecycle hooks | ABI/Swift hooks for background, foreground, low memory, network change; unit tests cover behavior. | Implemented; host-app/device wiring unverified |
| Browser integration helpers | Local gateway URL, `ipfs://`/`ipns://`/gateway-style URL mapping helpers, preload/cancel with path/URI/bare-CID normalization, cache stats/control. | Swift compile/link verified in simulator smoke; app integration unverified |
| Live ENS-backed smoke | `make live-smoke` resolves `vitalik.eth` and `daicowtf.eth` at runtime, mounts the online IPNS/DNSLink resolver, and fetches through the local gateway. | Done on Linux |
| Public CID corpus | Small checked-in corpus and opt-in smoke. | Started; should grow |
| Kubo parity | Deterministic Kubo-generated UnixFS/HAMT/range/directory-index parity tests. | Started; should grow |
| Long-running soak | Local cached-gateway RSS soak exists and passes. | Host-side only; device/network soak missing |
| Security parser/network limits | Tests cover oversized/malformed routing, traversal, invalid blocks, redirects, IPNS tamper/expiry, recursion, provider fanout. | Good MVP coverage |

## Remaining Work To Close The Goal

1. Expand the simulator smoke into an app-rendering check if needed by the Freedom browser integration.
2. Integrate the Swift wrapper into the Freedom iOS app and wire background/foreground, low-memory, and network-path events.
3. Profile real devices with Bee running beside this node and record RSS, CPU, startup, active retrieval, and idle network behavior.
4. Expand the public corpus with more stable IPFS/IPNS/DNSLink paths and larger/range-media cases.
5. Add longer host and device soaks for live retrieval, not just cached local reads.

## Completion Rule

The goal can be marked complete only after the device/app integration gates pass and the resource profile shows the node is usable beside Bee, or the spec is explicitly revised to remove those requirements.
