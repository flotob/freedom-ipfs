# RUST-WEB-002: ipfs.tech page assets intermittently fail with gateway 502/504

## Status

Mitigated on `agent/mobile-web-reliability-and-latency`.

The gateway no longer creates a fresh Bitswap swarm for each missing block.
`HttpRetriever` now keeps one shared, bounded Bitswap swarm per retriever so page
loads can reuse provider connections across root HTML and asset block reads. The
shared client also routes incoming Bitswap streams to active block requests,
which preserved the behavior that the previous one-shot swarms relied on.

The harness now supports repeat/fresh-gateway measurement and groups failures by
status/error, making this class of regression visible without hand-counting
terminal output.

## Scenario

The mobile app needs IPFS/IPNS websites to load as full browser pages, not only
as single root documents. `ipfs.tech` is a useful live page because it resolves
through DNSLink/IPNS and then fans out into many same-origin Nuxt JS chunks.

The harness emulates the iOS scheme handler's path semantics: root-relative page
assets under `/ipns/ipfs.tech/` are fetched as `/ipns/ipfs.tech/<asset>`.

## Reproduction

```sh
cargo run -p mobile-web-harness -- --case ipfs-tech-page-assets
```

The broader corpus also reproduces it:

```sh
cargo run -p mobile-web-harness -- --output /tmp/mobile-web-run.json
```

## Observed

The focused case can pass, but repeated cold runs fail often enough to matter for
mobile browsing. On 2026-05-03, the full corpus failed with 2 of 32 assets:

```text
FAIL ipfs-tech-page-assets
  - crawl had 2 failed assets, allowed 0
  assets: discovered=32 fetched=32 passed=30 failed=2
    - /ipns/ipfs.tech/_nuxt/DIs1UAle.js -> 504 after 35433ms
    - /ipns/ipfs.tech/_nuxt/AKg0Znx-.js -> 504 after 10576ms
```

A focused run with `--asset-concurrency 2` still failed with 3 of 32 assets,
including one fast `502`, so this is not only an over-wide page fan-out problem:

```text
FAIL ipfs-tech-page-assets --asset-concurrency 2
  - /ipns/ipfs.tech/_nuxt/Duo5E1ke.js -> 504 after 45247ms
  - /ipns/ipfs.tech/_nuxt/CFmqYC7r.js -> 502 after 313ms
  - /ipns/ipfs.tech/_nuxt/AKg0Znx-.js -> 504 after 10455ms
```

Repeat-mode baseline on this branch before the retrieval fix:

```text
cargo run -p mobile-web-harness -- \
  --case ipfs-tech-page-assets \
  --repeat 5 \
  --fresh-gateway-per-run \
  --output /tmp/ipfs-tech-cold-before.json

passed=0 failed=5 pass_rate=0.0%
root_ttfb p50=11747ms p90=12193ms p95=12193ms max=12193ms
asset_ttfb p50=5593ms p90=12510ms p95=30638ms max=42600ms
failed asset kinds: script=11, stylesheet=2
```

After keeping a shared Bitswap swarm per retriever and aligning the harness
defaults to 8 gateway requests and 6 asset fetches:

```text
cargo run -p mobile-web-harness -- \
  --case ipfs-tech-page-assets \
  --repeat 5 \
  --fresh-gateway-per-run \
  --asset-concurrency 6 \
  --output /tmp/ipfs-tech-cold-after-shared-asset6-5.json

passed=5 failed=0 pass_rate=100.0%
root_ttfb p50=11461ms p90=13639ms p95=13639ms max=13639ms
asset_ttfb p50=5610ms p90=11383ms p95=11527ms max=14639ms
measured run totals: 37036ms, 34941ms, 34842ms, 37692ms, 35684ms
```

An extended 20-run cold check showed the remaining public-network tail:

```text
cargo run -p mobile-web-harness -- \
  --case ipfs-tech-page-assets \
  --repeat 20 \
  --fresh-gateway-per-run \
  --asset-concurrency 6 \
  --output /tmp/ipfs-tech-cold-after-shared-asset6-20.json

passed=19 failed=1 pass_rate=95.0%
root_ttfb p50=11781ms p90=13656ms p95=13917ms max=45822ms
asset_ttfb p50=5605ms p90=11245ms p95=11428ms max=40045ms
```

Warm-cache behavior against one reused gateway remained fast:

```text
cargo run -p mobile-web-harness -- \
  --case ipfs-tech-page-assets \
  --warmup-runs 1 \
  --repeat 5 \
  --output /tmp/ipfs-tech-warm-after-shared-default6.json

passed=5 failed=0 pass_rate=100.0%
root_ttfb p50=25ms p90=40ms p95=40ms max=40ms
asset_ttfb p50=32ms p90=104ms p95=134ms max=236ms
measured run totals: 348ms, 330ms, 341ms, 354ms, 307ms
```

## Expected

All same-origin JS/CSS/image/font/media assets discovered from a reachable page
should return a 2xx response with a browser-appropriate MIME type. Cold loads can
be slower than warm loads, but a static site should not randomly lose JS chunks.

## Impact

For the iOS app this shows up as pages that render blank, partially styled, or
with broken client-side navigation. It is exactly the class of failure that makes
the Rust node feel "not yet Kubo-like" even when the root HTML request succeeds.

## Notes For Root Cause Work

- This was reproduced against the standalone Rust gateway, independent of iOS.
- Reducing harness asset concurrency from 4 to 2 did not eliminate failures.
- The failed URLs vary between runs, suggesting provider/retrieval reliability,
  timeout behavior, retry/fallback behavior, or cache/provider coalescing rather
  than one permanently bad path.
- The main confirmed issue was Bitswap churn: every cold missing block built a
  new libp2p swarm, redialed providers, and discarded any useful connections
  immediately after the block request. Full-page asset fan-out amplified this
  into flaky `504` responses.
- Marking every Bitswap peer as bad after one block timeout was also too
  aggressive for page workloads, because a timeout in one short-lived swarm does
  not prove that provider is bad for all nearby blocks.
- A later fix should reduce cold full-page time substantially. Reliability is
  better and asset fan-out is much less flaky, but ~35-40s cold `ipfs.tech`
  loads plus occasional root-provider timeouts are still not a mobile-quality
  target.

## 2026-05-03 Latency Follow-Up

Hypothesis:
Cold full-page time was dominated by per-block Bitswap behavior, not DNSLink,
provider lookup, SQLite, MIME detection, or gateway queueing.

Change:
Added opt-in gateway JSONL tracing and harness `--trace-output` collection, then
used the trace to make two retrieval changes:

- Bounded in-flight block fetch coalescing keyed by CID, with an 8s hedge so one
  stuck leader cannot hold every waiter until the 45s Bitswap timeout.
- Short-lived successful Bitswap peer preference. Once a peer serves a block,
  later block requests in the same process try that peer without a preliminary
  `WANT_HAVE`; unknown peers keep the conservative `WANT_HAVE` flow.

The harness also gained `--gateway-db` so fresh gateway processes can be
measured against the same persistent SQLite cache.

Commands:

```sh
cargo run -p mobile-web-harness -- \
  --case ipfs-tech-page-assets \
  --repeat 5 \
  --fresh-gateway-per-run \
  --asset-concurrency 6 \
  --trace-output /tmp/ipfs-tech-current-5-trace.jsonl \
  --output /tmp/ipfs-tech-current-5.json

cargo run -p mobile-web-harness -- \
  --case ipfs-tech-page-assets \
  --warmup-runs 1 \
  --repeat 5 \
  --asset-concurrency 6 \
  --trace-output /tmp/ipfs-tech-current-warm-trace.jsonl \
  --output /tmp/ipfs-tech-current-warm.json
```

Before:

```text
passed=5 failed=0 pass_rate=100.0%
root_ttfb p50=11461ms p90=13639ms p95=13639ms max=13639ms
asset_ttfb p50=5610ms p90=11383ms p95=11527ms max=14639ms
measured run totals: 37036ms, 34941ms, 34842ms, 37692ms, 35684ms
```

After:

```text
passed=5 failed=0 pass_rate=100.0%
root_ttfb p50=7265ms p90=7419ms p95=7419ms max=7419ms
asset_ttfb p50=633ms p90=2047ms p95=2396ms max=12042ms
measured run totals: 12466ms, 19318ms, 11104ms, 10648ms, 11372ms
```

Warm after one warmup remained fast:

```text
passed=5 failed=0 pass_rate=100.0%
root_ttfb p50=37ms p90=47ms p95=47ms max=47ms
asset_ttfb p50=39ms p90=106ms p95=144ms max=278ms
measured run totals: 342ms, 427ms, 407ms, 331ms, 266ms
```

Fresh process with persistent warm store:

```sh
rm -f /tmp/freedom-ipfs-ipfs-tech-persistent.db
cargo run -p mobile-web-harness -- \
  --case ipfs-tech-page-assets \
  --warmup-runs 1 \
  --repeat 3 \
  --fresh-gateway-per-run \
  --asset-concurrency 6 \
  --gateway-db /tmp/freedom-ipfs-ipfs-tech-persistent.db \
  --trace-output /tmp/ipfs-tech-persistent-warm-3-trace.jsonl \
  --output /tmp/ipfs-tech-persistent-warm-3.json
```

```text
passed=3 failed=0 pass_rate=100.0%
root_ttfb p50=91ms p90=198ms p95=198ms max=198ms
asset_ttfb p50=32ms p90=87ms p95=121ms max=216ms
measured run totals: 428ms, 363ms, 484ms
```

Trace evidence:

```text
before trace smoke:
bitswap_fetch count=26 total=143051ms p50=5602ms p95=6190ms max=6896ms
one shared directory/data CID fetched over Bitswap 6 times

after trace repeat:
bitswap_fetch count=105 total=134163ms p50=810ms p95=5605ms max=6608ms
hot shared CIDs are fetched once per fresh gateway run
```

Resource impact:
The changes keep existing caps: gateway request concurrency remains 8, asset
concurrency remains harness-side, Bitswap connection limits are unchanged, and
in-flight block coalescing is capped at 256 CIDs with hedged waiters. The
fresh-process persistent warm-store `ipfs.tech` run produced a 2.1 MiB SQLite
cache DB for the warmed page.

Failed experiments:

- Global `WANT_HAVE` timeout reductions to 750ms and 2s made asset samples fast
  but caused repeated root failures when delegated providers were stale. Reverted.
- One optimistic direct `WANT_BLOCK` attempt for an all-unknown peer set also
  regressed reliability: `ipfs-tech-page-assets` fresh repeat=5 passed 4/5,
  with one root 504 at 30.8s and run totals 14.3-30.8s. Reverted.
- Reusing the resolved UnixFS file CID for MIME/range/stream reads avoided
  repeated path walks in a deterministic gateway test, but live `ipfs.tech`
  evidence was worse: two fresh repeat=3 runs passed 1/3 and 2/3 with root
  504s, while the pushed `ac77017` code passed 3/3 in the same window. Reverted.
- Failure-only light-DHT fallback after a stale delegated provider set added
  about 10s to failed roots and did not recover `ipfs.tech` during the test
  window. Reverted.

Kubo comparison:

Kubo v0.41.0 lowpower/auto in a fresh temp repo, same harness:

```text
ipfs-tech-page-assets repeat=3:
passed=3 failed=0
run totals: 3087ms, 36ms, 32ms
root_ttfb p50=2ms p90=2449ms max=2449ms
asset_ttfb p50=3ms p90=103ms max=208ms

additional mobile-web corpus cases repeat=3:
passed=3 failed=0
run totals: 6509ms, 11ms, 12ms
vitalik-root-html-range root_ttfb p50=4ms p90=3781ms max=3781ms
ipfs-tech-developers-hero-range root_ttfb p50=3ms p90=2369ms max=2369ms
wikipedia-on-ipfs-root root_ttfb p50=4ms p90=357ms max=357ms
```

Conclusion:
Rust cold full-page latency is now materially better for this case, but Kubo is
still much faster on the first load and dramatically faster once its repo is
warm. Remaining evidence points to provider quality/session behavior and root
UnixFS path startup cost.
