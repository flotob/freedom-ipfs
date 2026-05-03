# RUST-WEB-001: Root UnixFS HTML File Served As Octet Stream

Status: fixed-lab
First seen: ca99de4
Still present on upstream: 329ccfe
Component: gateway MIME
Severity: browser-blocking

## Repro

`daicowtf.eth` resolves through ENS to:

```text
ipfs://QmWWbDw6kriAjRLyJNaeXjqqoyNn6kA18C42ubVzYKS4wS
```

Against the standalone or iOS-embedded gateway:

```sh
curl -i --range 0-8191 \
  http://127.0.0.1:<port>/ipfs/QmWWbDw6kriAjRLyJNaeXjqqoyNn6kA18C42ubVzYKS4wS/
```

## Expected

```text
content-type: text/html
```

## Actual

```text
content-type: application/octet-stream
```

The body starts with `<!DOCTYPE html>`, so WebKit receives real HTML bytes but
does not get HTML MIME semantics. This can present as a blank page in the
mobile browser even though retrieval succeeded.

## Root Cause

The gateway inferred MIME from the served UnixFS path. For a CID whose root is
itself a UnixFS file, the served path is empty, so `mime_guess` has no filename
extension and falls back to `application/octet-stream`.

## Lab Fix

When extension-based MIME detection cannot produce a type, sniff the first
bytes of the served file for obvious HTML document prefixes and return
`text/html`.

## Lab Verification

Focused regression:

```sh
cargo test -p freedom-ipfs-gateway serves_root_html_file_with_sniffed_mime_type -- --nocapture
```

Gateway unit suite:

```sh
cargo test -p freedom-ipfs-gateway --lib
```

Standalone black-box check from this branch:

```text
GET /ipfs/QmWWbDw6kriAjRLyJNaeXjqqoyNn6kA18C42ubVzYKS4wS/
Range: bytes=0-256

206 Partial Content
content-type: text/html
content-range: bytes 0-256/403507
```

Cold TTFB was still around 7.1s in that black-box run. Track retrieval latency
separately; this finding only covers MIME correctness.
