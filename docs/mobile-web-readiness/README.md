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
single-run harness.

The default live corpus is `tools/mobile-web-harness/corpus/mobile-web.json`.
It captures browser-facing checks such as status, MIME type, byte ranges,
minimum body size, body snippets, TTFB, and total response time.

Entries can also enable a page crawl. A crawl fetches the root HTML, extracts
same-origin browser subresources from HTML and CSS, resolves root-relative paths
as the iOS `ipfs://` / `ipns://` scheme handler would, and checks each asset's
status, MIME type, byte count, and timing.

## Findings

- `RUST-WEB-001`: root UnixFS HTML could be served as `application/octet-stream`
  when the path lacked an extension. Fixed in this branch with a regression test.
- `RUST-WEB-002`: `ipfs.tech` page asset crawls intermittently lose JS chunks to
  gateway `502` / `504` responses. Open.

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
