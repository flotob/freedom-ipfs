# Completion Audit

Last audited: 2026-05-03

Repository: `github.com/flotob/freedom-ipfs`

Implementation audited head: current repository head; latest Apple-platform implementation/workflow CI evidence is recorded below by head SHA

Spec: `/root/codex/mobile-rust-ipfs-node-spec.md`

## Verdict

The Linux-verifiable MVP implementation is substantially present and running, and the iOS XCFramework/simulator command-line plus app-rendering smoke gates now pass in GitHub Actions. The full thread goal is not complete yet because real-device integration and resource validation remain.

The remaining required evidence depends on the Freedom iOS app and target iPhones:

- Measure startup, idle RSS, CPU, network idleness, active retrieval, background/foreground, low-memory, network-change, and Bee co-residency behavior on target iPhones.
- Integrate the Swift wrapper into the browser app and wire app lifecycle/network-path events.
- Use `docs/ios-device-verification.md` as the device/app runbook and fill its evidence table.

Do not mark the overall implementation goal complete until the device/app gates pass.

## Current Verification Evidence

Fresh checks run against this file version:

```bash
cargo fmt --all --check && make verify
make live-smoke && make live-corpus
make live-soak
```

Apple-platform CI evidence:

```text
GitHub Actions iOS XCFramework run 25274438179 passed on 2026-05-03
Head SHA: 29bc53f09285e631bd140b70fde963a011693f7a
Job URL: https://github.com/flotob/freedom-ipfs/actions/runs/25274438179/job/74101967332
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
    - Per-target diagnostics: `retrieval_delta=cache_hits=5,http_provider_blocks=2,bitswap_blocks=0`; `routing_delta=delegated_lookups=2,delegated_results=46,delegated_errors=0,dht_lookups=0,dht_results=0,dht_errors=0`.
  - `daicowtf.eth` resolved to `/ipfs/bafybeidznfolm74c5cephzdycedx7hk76iawno45wemcvkflieotzo2lne`, returned `403507` bytes.
    - Per-target diagnostics: `retrieval_delta=cache_hits=20,http_provider_blocks=0,bitswap_blocks=3`; `routing_delta=delegated_lookups=3,delegated_results=5,delegated_errors=0,dht_lookups=0,dht_results=0,dht_errors=0`.
  - Retrieval stats: `cache_hits=25 http_provider_blocks=2 bitswap_blocks=3`.
  - Routing provider stats: `delegated_lookups=5 delegated_results=51 delegated_errors=0 dht_lookups=0 dht_results=0 dht_errors=0`.
- Public corpus smoke passed for the checked-in `vitalik-home`, `daicowtf-home`, `ipfs-tech`, `ipfs-tech-developers-hero`, `dist-ipfs-tech`, and `cid-ipfs-tech` entries, including a `bytes=0-127` range request for each entry:
  - `vitalik-home` returned `38394` bytes.
  - `daicowtf-home` returned `403507` bytes.
  - `ipfs-tech` returned `112239` bytes from `/ipns/ipfs.tech`.
  - `ipfs-tech-developers-hero` returned `184141` bytes from `/ipns/ipfs.tech/_nuxt/developers-hero.BRuJDQyf.jpg`.
  - `dist-ipfs-tech` returned `38953` bytes from `/ipns/dist.ipfs.tech`.
  - `cid-ipfs-tech` returned `24995` bytes from `/ipns/cid.ipfs.tech`.
  - Each range response returned `128` bytes, matched the full response prefix, and included a valid `Content-Range` header.
  - Retrieval stats: `cache_hits=132 http_provider_blocks=3 bitswap_blocks=10`.
- The opt-in live harnesses retry transient local-gateway `408`, `502`, `503`, and `504` responses so temporary public-network provider timeouts do not fail the first attempt when later attempts succeed.
- Kubo parity passed for generated CIDv1/raw-leaf UnixFS site files, CIDv0/DAG-PB UnixFS site files, directory-index fallback, range reads, and HAMT directories.
- Local cached-gateway soak passed: `500` requests, Linux RSS from `8960` KiB to `13312` KiB.
- Host live-retrieval soak passed: `2` cold gateway rounds against `vitalik-home` and `daicowtf-home`, `883802` total bytes, retrieval stats `cache_hits=50 http_provider_blocks=4 bitswap_blocks=6`, Linux RSS from `11264` KiB to `42240` KiB.
- GitHub Actions `iOS XCFramework` passed on `macos-15` with Xcode 16.4 using `actions/checkout@v6` and `actions/upload-artifact@v7`, both Node 24-backed releases. It built the real `FreedomIpfs.xcframework`, verified headers/module maps/exported C symbols, including the routing restart export, booted an iOS simulator, compiled and linked the Swift wrapper smoke, asserted that Swift gateway start rejects a non-loopback bind address, started the local gateway in the simulator via `simctl spawn booted`, fetched the CAR fixture through loopback, stopped the gateway, built and installed a generated UIKit/WebKit simulator app, imported the same CAR fixture, started the gateway from app process, fetched `/ipfs/{cid}` through loopback with `URLSession`, rendered the HTML in `WKWebView`, verified the DOM marker with JavaScript, and uploaded `FreedomIpfs.xcframework` as artifact ID `6769433178` (`60044527` bytes).
- `xtask build-xcframework` and `xtask verify-xcframework` still correctly refuse to run on Linux with the macOS/Xcode requirement message.

## Prompt-To-Artifact Checklist

| Requirement | Evidence | Status |
|---|---|---|
| Create `github.com/flotob/freedom-ipfs` | Git remote is `https://github.com/flotob/freedom-ipfs`; commits are pushed to `main`. | Done |
| Dual MIT/Apache-2.0 licensing | `LICENSE-MIT`, `LICENSE-APACHE`, workspace license `MIT OR Apache-2.0`. | Done |
| Rust workspace from scratch | Workspace crates exist for core, store, unixfs, namesys, routing, retrieval, gateway, mobile, and `xtask`. | Done |
| Clean-room implementation discipline | No vendored IPFS implementation is present; existing projects are listed as mining targets in the spec. | Done |
| CIDv0/CIDv1 parse/format | `freedom-ipfs-core` unit tests cover CID round-trip and verification; store tests cover equivalent CIDv0/CIDv1 DAG-PB cache lookup. | Done |
| Block verification before cache/serve | Core verification, store verified insertion, retrieval invalid HTTP block rejection. | Done |
| Bounded SQLite cache | `freedom-ipfs-store` implements SQLite cache, LRU eviction, active block retention during streaming, CIDv0/CIDv1 DAG-PB alias keys, stats, clear, trim, provider and bad-provider caches. | Done |
| Memory hot cache | Store has a bounded 16 MiB in-memory hot block cache in front of SQLite; tests cover clear/trim removing hot entries. | Done |
| Default disk cache 256 MiB | CLI/mobile default to `256 * 1024 * 1024`. | Done |
| CAR import/export for fixtures/cache warmup | Core CAR parse/encode and store import/export tests; CLI/mobile import/export APIs. | Done |
| Max block guard | Core verifies blocks with a max block size path. | Done for MVP |
| UnixFS raw/dag-pb files | `freedom-ipfs-unixfs` tests cover raw and dag-pb files. | Done |
| Multi-block UnixFS raw leaves | Range-across-inline-and-linked-block tests plus Kubo CIDv1/raw-leaf parity. | Done |
| CIDv0 DAG-PB UnixFS compatibility | Kubo-generated CIDv0 DAG-PB parity test covers directory index fallback, nested paths, multi-block files, and byte ranges through the local gateway. | Done |
| Directories and path resolution | UnixFS directory tests and gateway directory index test. | Done |
| HAMT directories | UnixFS HAMT tests and Kubo-generated HAMT parity. | Done for basic real-world compatibility |
| Local HTTP gateway data plane | Gateway crate and binary serve `/health`, `/ipfs`, `/ipns`; live smoke uses the local gateway; mobile FFI start/restart rejects non-loopback bind addresses. | Done |
| Streaming responses | Gateway streams full responses in bounded chunks. | Done |
| Stream eviction guard | Gateway stream scopes retain blocks they read; SQLite LRU eviction and explicit trim skip retained blocks until the stream scope releases them. | Done |
| HTTP range support | Gateway range tests cover success, malformed ranges, and unsatisfiable ranges. | Done |
| Browser MIME behavior | Directory `index.html` fallback with path-based `text/html` test. | Done for MVP |
| Useful gateway status codes and browser error pages | Tests cover invalid path/range, not found/name not found, timeout, busy, traversal cases, `text/html` browser-facing error pages, and escaped error details. | Done |
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
| No Kubo RPC compatibility | Gateway tests assert `GET` and `POST` to `/api/v0/version`, `/api/v0/id`, `/api/v0/refs`, and `/webui` return `404 Not Found` and do not expose Kubo-style JSON RPC responses. | Done |
| Minimal Bitswap client | Deterministic in-process libp2p Bitswap tests verify retrieval, cache insert, want-have before want-block for multi-peer sessions, DONT_HAVE handling, cancel behavior, and no-listener client swarm construction. | Done for MVP |
| Bitswap transports | TCP/WebSocket/QUIC configured; supported address filtering tests. | Done |
| No block serving to peers | Retrieval implements client-only Bitswap path, does not expose a serving strategy, and host tests assert Bitswap client swarms have no listen addresses. | Done for host; packet-level/device audit still needed for final confidence |
| Light DHT fallback | Kademlia client mode provider lookup and IPNS record lookup, deterministic local DHT tests, and no-listener/client-mode swarm construction test. Ignored public DHT smoke requires `FREEDOM_IPFS_LIVE_DHT_CID` because the default live corpus CIDs returned zero public DHT providers. | Done for implementation; public DHT target evidence remains opportunistic |
| Lazy/idle network behavior | DHT swarms are per lookup; Bitswap sessions are bounded; host tests assert DHT and Bitswap client swarms start without listeners; no background maintenance loops are apparent. | Partially verified; device/network inspection still needed |
| Routing modes | `auto`, `delegated`, `light_dht`, `offline` paths exist across CLI/mobile/gateway constructors; C ABI and Swift wrapper expose explicit online-gateway restart for Settings-style routing changes. | Done |
| iOS-first C ABI | `freedom-ipfs-mobile`, `ffi/include/freedom_ipfs.h`, and lifecycle/cache/gateway/preload APIs exist; mobile gateway start/restart accepts only loopback socket addresses. | Done |
| Swift wrapper | `ffi/swift/FreedomIpfsReader.swift` exists with gateway start/stop/restart, routing-mode change, stats, cache, lifecycle, preload/cancel, multi-router, and local URL mapping helpers; GitHub Actions Swift simulator smoke compiled and linked it against the XCFramework, checked non-loopback bind rejection, and a generated UIKit/WebKit app rendered a local gateway fixture through `WKWebView`. | Done for simulator smoke; Freedom app integration unverified |
| XCFramework build skeleton | `xtask build-xcframework` builds iOS targets, packages headers/module map, checks artifact structure/exported symbols using Rust `llvm-nm`, and produced `FreedomIpfs.xcframework` in GitHub Actions. | Done in macOS CI |
| XCFramework verifier | `xtask verify-xcframework` checks slices, headers, module maps, exported symbols including the routing restart export, a simulator Swift command-line gateway smoke, and a generated simulator app-rendering smoke on macOS; GitHub Actions run `25274438179` passed. | Done in macOS CI |
| Simulator smoke link/start/render | `xtask verify-xcframework` builds a Swift simulator executable that checks non-loopback bind rejection, imports a generated CAR fixture, starts the local gateway, fetches the fixture through loopback, and stops the gateway using `simctl spawn booted`; it also builds, installs, and launches a generated UIKit/WebKit app that imports the fixture, serves it through the local gateway, and verifies `WKWebView` DOM content; GitHub Actions run `25274438179` passed. | Done in macOS CI |
| Real iPhone resource target under 60 MiB RSS beside Bee | Required by spec. | Missing, needs device |
| Lifecycle hooks | ABI/Swift hooks for background, foreground, low memory, network change; unit tests cover behavior. | Implemented; host-app/device wiring unverified |
| Device/app verification runbook | `docs/ios-device-verification.md` defines app wiring, real-device matrix, Bee co-residency runs, live content cases, range checks, lifecycle checks, and acceptance targets. | Done; evidence still missing |
| Browser integration helpers | Local gateway URL, `ipfs://`/`ipns://`/gateway-style URL mapping helpers, preload/cancel with path/URI/bare-CID normalization, cache stats/control. | Swift compile/link and generated `WKWebView` app smoke verified in simulator; Freedom app integration unverified |
| Live ENS-backed smoke | `make live-smoke` resolves `vitalik.eth` and `daicowtf.eth` at runtime, mounts the online IPNS/DNSLink resolver, fetches through the local gateway, and prints per-target retrieval/routing deltas that distinguish cache, HTTP-provider blocks, Bitswap blocks, delegated provider lookups, and light-DHT fallback. | Done on Linux |
| Public IPFS/IPNS corpus | Checked-in opt-in corpus covers the ENS-derived immutable `/ipfs` paths plus DNSLink-backed `/ipns` paths for `ipfs.tech`, one larger `ipfs.tech` media asset, `dist.ipfs.tech`, and `cid.ipfs.tech`; `make live-corpus` passed with full-response and `bytes=0-127` range checks. | Expanded; should still grow with more media cases |
| Kubo parity | Deterministic Kubo-generated CIDv1/raw-leaf UnixFS, CIDv0/DAG-PB UnixFS, HAMT, range, and directory-index parity tests. | Expanded |
| Long-running soak | Local cached-gateway RSS soak and host live-retrieval RSS soak exist and pass. | Host-side coverage improved; device soak missing |
| Security parser/network limits | Tests cover oversized/malformed routing, traversal, invalid blocks, redirects, IPNS tamper/expiry, recursion, provider fanout. | Good MVP coverage |

## Remaining Work To Close The Goal

1. Integrate the Swift wrapper into the Freedom iOS app and wire background/foreground, low-memory, and network-path events using `docs/ios-device-verification.md`.
2. Profile real devices with Bee running beside this node and record RSS, CPU, startup, active retrieval, and idle network behavior in the runbook evidence table.
3. Expand the public corpus with more larger media cases and documented pass/fail notes.
4. Find or control a stable public-DHT provider target for `FREEDOM_IPFS_LIVE_DHT_CID`.
5. Add device soaks for live retrieval, background/foreground, and memory growth.

## Completion Rule

The goal can be marked complete only after the device/app integration gates pass and the resource profile shows the node is usable beside Bee, or the spec is explicitly revised to remove those requirements.
