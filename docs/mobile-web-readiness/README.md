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

Or let it spawn the standalone gateway:

```sh
cargo build -p freedom-ipfs-gateway
cargo run -p mobile-web-harness -- --output /tmp/mobile-web-run.json
```

The default live corpus is `tools/mobile-web-harness/corpus/mobile-web.json`.
It captures browser-facing checks such as status, MIME type, byte ranges,
minimum body size, body snippets, TTFB, and total response time.

## Next Scenario Targets

- Full-page asset crawls: fetch the root HTML, extract same-origin CSS, JS,
  image, font, and media URLs, then verify status/MIME/range behavior for each
  subresource.
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
