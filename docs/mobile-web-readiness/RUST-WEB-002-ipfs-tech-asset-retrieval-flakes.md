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
spawned-gateway request cap with the gateway default of 8:

```text
cargo run -p mobile-web-harness -- \
  --case ipfs-tech-page-assets \
  --repeat 5 \
  --fresh-gateway-per-run \
  --output /tmp/ipfs-tech-cold-after-shared-default8-5.json

passed=5 failed=0 pass_rate=100.0%
root_ttfb p50=11772ms p90=11912ms p95=11912ms max=11912ms
asset_ttfb p50=5611ms p90=6161ms p95=11421ms max=11788ms
measured run totals: 45614ms, 46783ms, 46637ms, 46296ms, 46292ms
```

Warm-cache behavior against one reused gateway remained fast:

```text
cargo run -p mobile-web-harness -- \
  --case ipfs-tech-page-assets \
  --repeat 5 \
  --output /tmp/ipfs-tech-repeat-after-shared-default8.json

passed=5 failed=0 pass_rate=100.0%
first measured run total=43202ms
subsequent measured run totals: 355ms, 254ms, 354ms, 427ms
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
  better, but a ~46s cold `ipfs.tech` load is still not a mobile-quality target.
