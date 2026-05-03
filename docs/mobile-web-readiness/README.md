# Mobile Web Readiness Lab

This branch is a sidecar lab for black-box and regression testing of
`freedom-ipfs` as a read-only mobile web node. Keep `main` free for the
long-running upstream agent; use this branch to collect reproducible browser
compatibility bugs, local fixes, and scenarios that should survive rebases.

## Workflow

- Fetch `origin/main`, then merge it into this lab branch when refreshing.
- Record each concrete failure under `docs/mobile-web-readiness/`.
- Prefer a failing regression test before a fix when the bug is understood.
- Keep live-network scenarios opt-in; default tests should stay deterministic.

## Harness

The black-box harness lives in `tools/mobile-web-harness`.

Use an already-running gateway:

```sh
cargo run -p mobile-web-harness -- --gateway-url http://127.0.0.1:50017
```

Run one or more focused cases:

```sh
cargo run -p mobile-web-harness -- --case ipfs-tech-page-assets
```

Or let it spawn the standalone gateway:

```sh
cargo build -p freedom-ipfs-gateway
cargo run -p mobile-web-harness -- --output /tmp/mobile-web-run.json
```

Run a focused case repeatedly and write an aggregate JSON report:

```sh
cargo run -p mobile-web-harness -- \
  --case ipfs-tech-page-assets \
  --repeat 20 \
  --output /tmp/ipfs-tech-repeat.json
```

The repeat report includes measured pass/fail counts, pass rate, root and asset
TTFB/total-time p50/p90/p95/max summaries, failed asset kinds, and failed URLs
grouped by status/error. Use `--warmup-runs` to separate warm-cache behavior, or
`--fresh-gateway-per-run` when measuring repeated cold gateways. Spawning uses
the same routing, DHT, request-concurrency, and asset-concurrency knobs as the
single-run harness, with an 8-request gateway default and a 6-asset crawl
default to model bounded browser pressure.

For fresh-process warm-store measurements, pass `--gateway-db /tmp/cache.db`
while the harness is spawning the gateway. This forwards the path to the
gateway's SQLite cache. Combined with `--fresh-gateway-per-run --warmup-runs 1`,
the warmup process populates the DB and measured runs start new gateway
processes against the same persistent cache. On Linux, spawned-gateway RSS is
sampled from `/proc/<pid>/status` after each run and included in the JSON report.

For gateway phase tracing, pass `--trace-output /tmp/run.jsonl`. When the
harness spawns the Rust gateway it forwards this path to the gateway, parses the
JSONL events, and adds a phase summary to the report. This is the preferred way
to distinguish DNSLink/name resolution, provider lookup, Bitswap fetch, UnixFS
path traversal, MIME sniffing, and gateway limiter behavior during live runs.

The default live corpus is `tools/mobile-web-harness/corpus/mobile-web.json`.
It captures browser-facing checks such as status, MIME type, byte ranges,
minimum body size, body snippets, TTFB, and total response time. The corpus now
covers ENS-derived immutable HTML snapshots, DNSLink/IPNS page crawls, a
DNSLink image byte-range case, and a small independent Wikipedia-on-IPFS
DNSLink root.

Entries can also enable a page crawl. A crawl fetches the root HTML, extracts
same-origin browser subresources from HTML and CSS, resolves root-relative paths
as the iOS `ipfs://` / `ipns://` scheme handler would, and checks each asset's
status, MIME type, byte count, and timing.

## Findings

- `RUST-WEB-001`: root UnixFS HTML could be served as `application/octet-stream`
  when the path lacked an extension. Fixed in this branch with a regression test.
- `RUST-WEB-002`: `ipfs.tech` page asset crawls intermittently lost JS chunks to
  gateway `502` / `504` responses. Mitigated by the shared Bitswap client and
  harness repeat reporting. A latency follow-up added phase tracing, bounded
  in-flight block fetch coalescing, successful Bitswap peer preference, a
  conservative recent-peer race for slow provider-cache misses, and
  connection-ready Bitswap stream opening; fresh `ipfs.tech` 5-run totals
  improved from roughly 35-38s to roughly 10.6-19.3s while preserving 5/5 pass
  rate. A later same-window comparison reproduced the 30s root 504 tail on the
  previous pushed commit, then a connection-ready stream fix passed 3/3 with root
  max 10.9s and no `no addresses for peer` Bitswap errors.

## Next Scenario Targets

- ENS-to-CID controls: resolve `.eth` names outside the gateway, then test the
  resulting `/ipfs/<cid>/` path directly so ENS bugs stay separate from IPFS
  retrieval bugs.
- Range-heavy media fixtures: request first, middle, and suffix byte ranges from
  known audio/video CIDs and verify `206`, `Content-Range`, and bounded memory.
- Cold/warm timing pairs: run each case twice against the same gateway and record
  cache-hit speedups, provider lookup counts, and outlier latencies.
- Failure classification: distinguish name resolution failures, provider
  discovery failures, block retrieval failures, MIME bugs, and browser-origin
  incompatibilities in the report output.
