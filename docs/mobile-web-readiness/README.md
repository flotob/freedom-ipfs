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
