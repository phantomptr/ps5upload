# Link install: two modes, user-chosen

**Status:** approved 2026-09-21. Supersedes the single-path design in
[[url-install-parallel-proxy]] by adding a second mode rather than replacing it.

## Problem

Installing from an http(s) link runs at ~3 MB/s for a user whose download
manager pulls the same file, over the same line, at 40-50 MB/s.

Measured on that user's 5.31.4 run (48 windows, 1.5 GiB):

| Limiter | Value |
|---|---|
| Per connection | ~525 kB/s, uniform across all 8 |
| Our ceiling (8 x 525) | 4.20 MB/s |
| Reached inside a window | 3.04 MB/s (72% of ceiling) |
| Idle between windows | 16% |
| Delivered end to end | 2.56 MB/s |

Two facts bound the problem:

- **We do not saturate our own ceiling.** 39% is lost to our own scheduling:
  a per-window work queue that lets workers idle at the tail, and
  `readahead = 1`, which stops us fetching whenever the console pauses.
- **The ceiling itself is set by connection count.** Whether the 525 kB/s is a
  per-connection host cap or a TCP bandwidth-delay-product limit is NOT
  established; both make throughput scale with connections, which is why
  download managers open many.

What is NOT the problem: our fetch path. The same code moved 3,546 MB/s from a
local origin and completed a 101 GB install end to end at 103 MB/s.

## Design

Two modes for a link install, chosen by the user, plus one option.

### Mode 1 — Direct: the PS5 downloads it

Hand the URL to the DPI daemon; the console fetches and installs on its own.

**Hardware-validated 2026-09-21 on the Phat (FW 5.10):** downloaded at
**114.1 MB/s** from a LAN origin, produced the correct artifact
(fingerprint `002941e2...`), with our pkg-host serving zero requests.

- The console opens **2 connections** (`ScePlayGoCoreHttpReqThread_0/_1`).
- The PC is uninvolved after kickoff: it may sleep, close, or disconnect.
- Nothing is staged on either side.
- Routing through DPI avoids the in-process `InstallByPackage` hang on FW < 11
  (see [[stream-install-serve-only]]); that hang does not apply here.

**When it loses:** only 2 connections. Against a high-latency or throttled
origin it gets roughly a quarter of what Accelerated's 8 do. Expected, not
verified — the console's TCP stack may differ from ours.

### Mode 2 — Accelerated: this computer downloads it

The existing engine-side parallel proxy, with the three measured losses fixed:

1. **Continuous work queue.** Chunks are pulled from a queue that spans
   windows instead of resetting at each boundary, so a worker that finishes
   early starts the next window's work instead of idling. Targets the 28%.
2. **Deeper readahead.** Fetch further ahead of console demand so we are not
   idle whenever it pauses. Targets the 16%.
3. **Adaptive connection count.** Ramp up while aggregate throughput improves;
   back off on 429/503 (already handled) or when throughput stops rising.
   Remember the best count per host. This raises the ceiling AND measures it,
   which settles the per-connection-vs-per-IP question we could not answer.

**When it wins:** slow, distant or throttled sources — anywhere connection
count is the only available lever.

### Option — Skip certificate check

Per-install, default off, **Accelerated only**. `ureq`'s
`TlsConfig::disable_verification` is a supported flag.

Disabled with an explanation under Direct: the console performs its own TLS
handshake and we have no control over what it accepts.

For self-hosted origins, self-signed certificates, and hosts with expired
certificates. It means a machine in between could substitute the file, which
is why it is per-install and off by default. Same switch as `curl -k`.

### Choosing

- Plain-language labels: "Let the PS5 download it" / "Download through this
  computer". Not "DPI direct install".
- Remember the choice per host.
- If Direct is refused (firmware, headers required, certificate rejected),
  fall back to Accelerated with a message naming the reason.

### Not building

Auto-benchmarking both modes to pick one. Speculative complexity for a
decision made once per host.

## Testing

Off-console unit tests: mode selection, per-host memory, fallback-with-reason,
TLS flag plumbing, and the ramp controller against simulated origins (uniformly
slow, one-fast-rest-throttled, rate-limiting).

On-console: Direct is already validated. Accelerated is the shipping path;
re-verify after the scheduling changes.

## Open, and deliberately not blocking

- **Why 525 kB/s?** Host policy, TCP window/RTT, or our client identity (we
  send `User-Agent: ps5upload` and no other headers). The adaptive ramp
  measures the ceiling regardless of which it is.
- **Direct against a throttled origin** is unmeasured.
- The **launch failure** seen on this console (CE-100008-9) is a backport /
  fakelib issue, independent of installs and of this design.
