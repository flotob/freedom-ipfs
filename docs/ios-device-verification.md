# iOS Device Verification

This runbook is the remaining evidence gate for calling the first mobile reader usable in the Freedom browser. Simulator CI proves that the XCFramework links, starts a loopback gateway, and renders through `WKWebView`; it does not prove real-device memory, idle network behavior, lifecycle behavior, or Bee co-residency.

## Inputs

- Latest verified XCFramework artifact from `docs/status.md`.
- Freedom iOS app branch with Bee available in the same app process.
- At least two real iPhones: the oldest supported target device and one current target device.
- Xcode Instruments or `xctrace` access for memory, CPU, and network captures.

## App Wiring Checklist

Wire the Swift wrapper into the browser app before measuring:

- Create the reader with a persistent app data directory and the default `256 MiB` cache budget unless the app has a stricter product budget.
- Start the online gateway on `127.0.0.1:0` with routing mode `auto`, delegated routing left at the default unless testing failover, request concurrency capped to a small mobile budget such as `4`, and explicit DHT budgets such as `15` seconds and `8` providers.
- Keep ENS/contenthash resolution outside this node. Convert the resolved `/ipfs/...` or `/ipns/...` path through `FreedomIpfsReader.localGatewayURL(for:)` and load that loopback URL in the browser.
- Route direct `ipfs://`, `ipns://`, `/ipfs`, and `/ipns` inputs through the same local gateway mapping.
- Call `enterBackground()` from the app background notification.
- Call `enterForeground()` from the app foreground notification.
- Call `handleLowMemory(maxCacheBytes:)` from the app memory-warning path.
- Call `handleNetworkChange()` from the `NWPathMonitor` path when connectivity class changes.
- Start preload on committed navigation and cancel preload when navigation is cancelled or replaced.
- Log `diagnostics` around each measured navigation so device evidence records cache hits, HTTP-provider blocks, Bitswap blocks, delegated lookups, DHT fallback, cache size, gateway state, lifecycle state, and stuck preloads from one timestamped snapshot.
- Stop the gateway when the app tears down the node.

Minimal shape:

```swift
let reader = try FreedomIpfsReader(
    dataDirectory: cacheDirectory.appendingPathComponent("freedom-ipfs"),
    maxCacheBytes: 256 * 1024 * 1024
)

try reader.startOnlineGateway(
    routingMode: .auto,
    maxConcurrentRequests: 4,
    dhtQueryTimeoutSeconds: 15,
    dhtMaxProviders: 8
)

let localURL = reader.localGatewayURL(for: "/ipfs/bafy...")
let before = reader.diagnostics
```

## Device Matrix

Run each case with:

- Bee disabled, Freedom IPFS enabled.
- Bee enabled, Freedom IPFS enabled.
- Bee enabled, Freedom IPFS disabled as the baseline for app process deltas.

Record device model, iOS version, app commit, Freedom IPFS commit, Bee commit, routing mode, cache state, and network type.

## Test Cases

1. Cold launch and idle:
   - Install a clean app build.
   - Launch, create the node, start the gateway, and load no IPFS page.
   - Wait 60 seconds.
   - Record RSS, CPU, open sockets/listeners, and network bytes.

2. ENS-derived immutable content:
   - Resolve `vitalik.eth` outside the node.
   - Load the resulting `/ipfs/bafybeiaql2jo3fu5b7c4lmpoi5drh5sam7yt652shwdgwbky4o7uw33u2u` through the local gateway.
   - Repeat three times after clearing the Freedom IPFS cache.
   - Record first byte, complete load time, peak RSS, end RSS, CPU, network bytes, and `FreedomIpfsReader.diagnostics`.

3. Larger ENS-derived immutable content:
   - Resolve `daicowtf.eth` outside the node.
   - Load `/ipfs/bafybeidznfolm74c5cephzdycedx7hk76iawno45wemcvkflieotzo2lne` through the local gateway.
   - Repeat the same measurements as case 2.

4. DNSLink/IPNS content:
   - Load `/ipns/ipfs.tech`, `/ipns/dist.ipfs.tech`, and `/ipns/cid.ipfs.tech`.
   - Record successful render, first byte, complete load time, and whether fallback routing was needed from `diagnostics.routingStats`.

5. Byte-range behavior:
   - Load a page or media object that causes WebKit range requests, or issue `Range: bytes=0-127` against one known loaded path through the local gateway.
   - Verify `206 Partial Content`, a valid `Content-Range`, and stable memory while repeating the range request.

6. Idle network audit:
   - After a successful load, leave the app foregrounded and idle for 5 minutes.
   - Verify no sustained CPU work and no continuous network traffic.
   - Confirm there are no public inbound P2P listeners; the only expected listener is the local loopback gateway.

7. Background and foreground:
   - Load one IPFS page, background the app for 5 minutes, then foreground it.
   - Verify preloads are cancelled or quiesced using `diagnostics.activePreloadCount`, `diagnostics.isBackgrounded` reflects the lifecycle hook, memory does not grow while backgrounded, and foreground retrieval still works.

8. Low-memory path:
   - Trigger a memory warning from Xcode or the test harness.
   - Verify cache trimming runs, the process remains alive, and a subsequent cached or network load still succeeds.

9. Network change:
   - Start on Wi-Fi, load an IPFS page, switch to cellular or another network path, then load again.
   - Verify provider metadata is cleared, `diagnostics.routingStats` records fresh lookups after the path change, and retrieval recovers without restarting the app.

10. Repeated retrieval soak:
    - Run 20 alternating loads of `vitalik.eth`, `daicowtf.eth`, and one `/ipns` path.
    - Clear cache every fifth run.
    - Record RSS before, peak, and after the run.

## Acceptance Targets

The first product-usable release should meet these targets on every target device:

- App RSS delta for Freedom IPFS beside Bee is under `60 MiB` after 60 seconds idle.
- No sustained idle CPU above `1%` after retrieval settles.
- No continuous idle network traffic after name/routing caches settle.
- Local gateway remains loopback-only.
- No block serving, content providing, DHT server mode, or Kubo RPC surface is visible on device.
- `vitalik.eth`, `daicowtf.eth`, and the three checked-in `/ipns` paths render through the local gateway.
- `diagnostics.retrievalStats` and `diagnostics.routingStats` show non-zero retrieval/routing work for cold network loads, and `diagnostics.activePreloadCount` returns to zero after cancelled or completed preloads.
- Routing can be changed from `auto` to `delegated` or `light_dht` with `setRoutingMode(...)` or `restartOnlineGateway(...)`; active preloads are cancelled and a new loopback gateway URL is surfaced to the app.
- Background, foreground, low-memory, and network-change hooks run without process death or stuck retrieval.
- Repeated retrieval soak does not show unbounded RSS growth.

If a device misses a target, keep the project open and record the failure with logs, trace files, and the smallest repro path.

## Evidence Template

| Case | Device | Bee | Cache | Result | RSS idle delta | RSS peak | CPU idle | Network idle | Retrieval delta | Routing delta | Active preloads | Notes |
|---|---|---|---|---|---:|---:|---:|---:|---|---|---:|---|
| cold idle |  | on/off | clean/warm | pass/fail |  |  |  |  |  |  |  |  |
| vitalik.eth |  | on/off | clean/warm | pass/fail |  |  |  |  |  |  |  |  |
| daicowtf.eth |  | on/off | clean/warm | pass/fail |  |  |  |  |  |  |  |  |
| DNSLink/IPNS |  | on/off | clean/warm | pass/fail |  |  |  |  |  |  |  |  |
| byte range |  | on/off | clean/warm | pass/fail |  |  |  |  |  |  |  |  |
| background |  | on/off | warm | pass/fail |  |  |  |  |  |  |  |  |
| low memory |  | on/off | warm | pass/fail |  |  |  |  |  |  |  |  |
| network change |  | on/off | warm | pass/fail |  |  |  |  |  |  |  |  |
| soak |  | on/off | mixed | pass/fail |  |  |  |  |  |  |  |  |

When this table is filled with passing evidence and linked traces, update `docs/completion-audit.md` and only then mark the overall implementation goal complete.
