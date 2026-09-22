# Link Install Modes Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user choose how a link install downloads — the PS5 fetching it
directly, or this computer fetching it with adaptive parallelism — and allow
skipping certificate checks on the computer-fetched path.

**Architecture:** Direct mode routes the URL to the already-working
`/api/pkg/dpi-install` endpoint (hardware-validated at 114 MB/s). Accelerated
mode keeps the engine-side proxy and replaces its per-window worker scheme with
one bounded pool pulling from a queue that spans the readahead horizon, with the
pool size adapting to measured throughput.

**Tech Stack:** Rust (engine, ureq 3.4), TypeScript/React + zustand (client),
Tauri command bridge.

**Spec:** `docs/superpowers/specs/2026-09-21-link-install-modes-design.md`

## Global Constraints

- Rust edition 2021, toolchain 1.98.1. `cargo fmt` after EVERY Rust edit — the
  validate gate fails on formatting.
- The engine must still compile for `aarch64-linux-android`, where
  `remote_pkg` is cfg'd out and `RemotePkg` is an uninhabited enum: any new
  public method needs an Android stub or the build breaks.
- Every new user-facing string goes in `client/src/i18n/locales/en.ts` AND all
  19 other locales, or the i18n coverage gate fails. Locale files are generated
  at column 0 — never reformat them.
- The install URL is never logged, never put in a task payload or diagnostic
  bundle, and `RemoteSource`'s `Debug` omits it. Preserve that.
- Connection count stays within `1..=32` unless a task says otherwise.

---

### Task 1: One bounded worker pool across the readahead horizon

Today each window fetch spawns its own workers, and readahead spawns a whole
separate `fetch_window` on its own thread — so raising readahead multiplies
connections instead of keeping them busy. Replace both with a single pool.

**Files:**
- Modify: `engine/crates/ps5upload-engine/src/remote_pkg.rs`
- Test: same file, `mod origin_tests`

**Interfaces:**
- Consumes: `chunk_ranges(start, len) -> Vec<(u64, u64)>`, `fetch_piece(&agent, start, len, dst) -> Result<PieceStat, String>`, `build_agent(parallelism) -> ureq::Agent`.
- Produces: `RemoteSource::active_conns() -> usize` (used by Task 2).

- [ ] **Step 1: Write the failing test**

```rust
    /// Readahead must not multiply connections. With a pool, fetching several
    /// windows ahead uses the SAME sockets; with a thread per window it opens
    /// parallelism x windows.
    #[test]
    fn readahead_reuses_the_pool_instead_of_multiplying_connections() {
        let total = 16 * 1024 * 1024u64;
        let sockets = Arc::new(AtomicUsize::new(0));
        let addr = spawn_keepalive_origin(body(total as usize), sockets.clone());
        let mut src = RemoteSource::new(format!("http://{addr}/game.pkg"), total);
        src.window_bytes = 4 * 1024 * 1024;
        src.parallelism = 4;
        src.readahead = 3;
        src.worker_agents = (0..4).map(|_| Mutex::new(build_agent(1))).collect();
        for w in 0..4u64 {
            let s = w * src.window_bytes;
            src.read_range(s, s + src.window_bytes - 1).expect("window");
        }
        let n = sockets.load(Ordering::SeqCst);
        assert!(
            n <= 4,
            "origin saw {n} sockets for a pool of 4; readahead is spawning its own workers"
        );
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd engine && cargo test -q -p ps5upload-engine --lib readahead_reuses_the_pool`
Expected: FAIL — more sockets than the pool size, because `prefetch_after`
spawns a thread that calls `fetch_window`, which builds its own workers.

- [ ] **Step 3: Replace per-window workers with a pool**

Give `RemoteSource` a queue of outstanding chunks spanning the readahead
horizon, and have exactly `parallelism` long-lived workers drain it. Keep
`fetch_window`'s signature so callers are unchanged; it now enqueues the
window's chunks and waits for that window's own completion counter.

```rust
struct ChunkJob {
    /// Absolute file offset and length of this chunk.
    start: u64,
    len: u64,
    /// Which window it belongs to, so a waiter knows when its window is whole.
    window: u64,
}

/// One window's in-flight state: where its bytes land and how many chunks
/// are still outstanding. `fetch_window` waits on `remaining == 0`.
struct WindowSlot {
    buf: Vec<u8>,
    remaining: usize,
    error: Option<String>,
}
```

Worker loop (one per pool slot, started once and kept for the source's life):

```rust
loop {
    let job = {
        let mut q = self.chunk_queue.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            // Lowest offset first: the console reads forward, so serving its
            // next request beats finishing a readahead window.
            if let Some(k) = q.keys().next().copied() {
                break q.remove(&k);
            }
            if self.stopping.load(Ordering::Relaxed) { return; }
            q = self.queue_ready.wait(q).unwrap_or_else(|e| e.into_inner());
        }
    };
    let Some(job) = job else { continue };
    let res = self.fetch_piece(&agent, job.start, job.len, &mut scratch[..job.len as usize]);
    self.complete_chunk(job, res, &scratch);
}
```

`complete_chunk` copies the bytes into that window's `buf`, decrements
`remaining`, records the first error, and notifies the window's condvar.

```rust
    /// Chunks awaiting a worker, across every window currently in flight.
    /// Ordered by file offset so the console's sequential reads are served
    /// first even when readahead has queued later windows behind them.
    chunk_queue: Mutex<std::collections::BTreeMap<u64, ChunkJob>>,
    queue_ready: std::sync::Condvar,
```

Workers block on `queue_ready`, take the lowest-offset job, fetch it with
their own agent, and mark it done. `fetch_window` enqueues and waits on the
window's own completion counter.

- [ ] **Step 4: Run the test and the whole suite**

Run: `cd engine && cargo fmt --all && cargo test -q -p ps5upload-engine --lib`
Expected: the new test PASSES and all pre-existing origin tests still pass —
especially `connections_are_reused_across_windows`,
`a_throttled_connection_does_not_set_the_window_pace` and
`a_throttled_connection_is_dropped_and_reopened`.

- [ ] **Step 5: Raise the readahead default**

```rust
/// Windows to keep in flight ahead of console demand. Was 1, which left the
/// fetcher idle 16% of a measured install: it finished a window and waited for
/// the console to ask before starting the next. With one bounded pool (see
/// `chunk_queue`) a deeper horizon costs no extra connections.
const DEFAULT_READAHEAD_WINDOWS: u64 = 4;
```

- [ ] **Step 6: Commit**

```bash
cd engine && cargo fmt --all && cd ..
git add engine/crates/ps5upload-engine/src/remote_pkg.rs
git commit -m "perf(install): one worker pool across the readahead horizon"
```

---

### Task 2: Adapt the connection count to measured throughput

**Files:**
- Modify: `engine/crates/ps5upload-engine/src/remote_pkg.rs`
- Test: same file, `mod origin_tests`

**Interfaces:**
- Consumes: `RemoteSource::active_conns()` from Task 1.
- Produces: `conns=` in the `url-install origin fetch` log line reports the
  live count rather than a constant.

- [ ] **Step 1: Write the failing test**

```rust
    /// The pool must grow when growing helps. This origin serves every
    /// connection at the same modest rate, so aggregate throughput rises
    /// roughly linearly with connection count — exactly the case where a fixed
    /// 8 leaves speed on the table.
    #[test]
    fn the_pool_grows_while_throughput_improves() {
        let total = 32 * 1024 * 1024u64;
        let sockets = Arc::new(AtomicUsize::new(0));
        let addr = spawn_throttled_origin(body(total as usize), sockets.clone(), 40);
        let mut src = RemoteSource::new(format!("http://{addr}/game.pkg"), total);
        src.window_bytes = 8 * 1024 * 1024;
        src.parallelism = 2;
        src.max_parallelism = 16;
        for w in 0..4u64 {
            let s = w * src.window_bytes;
            src.read_range(s, s + src.window_bytes - 1).expect("window");
        }
        assert!(
            src.active_conns() > 2,
            "pool stayed at {} against an origin where more connections pay",
            src.active_conns()
        );
    }
```

`spawn_throttled_origin(data, sockets, ms_per_mib)` is a new helper modelled on
`spawn_keepalive_origin`, sleeping `ms_per_mib` per MiB served per connection.

- [ ] **Step 2: Run it and watch it fail**

Run: `cd engine && cargo test -q -p ps5upload-engine --lib the_pool_grows_while`
Expected: FAIL — `active_conns()` is still 2; nothing adjusts it.

- [ ] **Step 3: Implement the ramp**

After each window, compare its aggregate rate with the previous window's. Grow
by 2 while the rate improved by more than 10%; shrink by 2 if it fell by more
than 10% or the window saw any `rate_limited`. Clamp to `1..=max_parallelism`.

```rust
    /// Live connection count, adjusted per window by `adapt_parallelism`.
    active: AtomicUsize,
    /// Upper bound for the ramp. 32 by default, `PS5UPLOAD_URL_THREADS` pins it.
    max_parallelism: usize,
    /// Aggregate MB/s of the previous window, for the improve/regress test.
    last_window_mbps: Mutex<f64>,
```

```rust
    fn adapt_parallelism(&self, mbps: f64, rate_limited: u32) {
        let mut prev = self.last_window_mbps.lock().unwrap_or_else(|e| e.into_inner());
        let cur = self.active.load(Ordering::Relaxed);
        let next = if rate_limited > 0 || (*prev > 0.0 && mbps < *prev * 0.9) {
            cur.saturating_sub(2).max(1)
        } else if *prev == 0.0 || mbps > *prev * 1.1 {
            (cur + 2).min(self.max_parallelism)
        } else {
            cur
        };
        *prev = mbps;
        self.active.store(next, Ordering::Relaxed);
    }
```

- [ ] **Step 4: Run the test and the suite**

Run: `cd engine && cargo fmt --all && cargo test -q -p ps5upload-engine --lib`
Expected: new test PASSES; `a_uniformly_slow_origin_is_not_fought_with_reconnections`
still passes (rotation and the ramp are independent).

- [ ] **Step 5: Commit**

```bash
cd engine && cargo fmt --all && cd ..
git add engine/crates/ps5upload-engine/src/remote_pkg.rs
git commit -m "perf(install): adapt the connection count to measured throughput"
```

---

### Task 3: Skip-certificate-check option through to the fetcher

**Files:**
- Modify: `engine/crates/ps5upload-engine/src/remote_pkg.rs`
- Modify: `engine/crates/ps5upload-engine/src/pkg_install.rs`
- Test: `engine/crates/ps5upload-engine/src/remote_pkg.rs`, `mod tests`

**Interfaces:**
- Consumes: `build_agent(parallelism)`.
- Produces: `RemoteSource::new_with_options(url, total_size, insecure_tls: bool)`;
  `InstallStartRequest.insecure_tls: bool` (serde default false).

- [ ] **Step 1: Write the failing test**

```rust
    /// The flag must reach the TLS config. We assert the plumbing, not a live
    /// handshake: a bad-certificate server is not worth standing up here.
    #[test]
    fn insecure_tls_is_off_unless_asked_for() {
        let a = RemoteSource::new("https://h.example/g.pkg".into(), 1);
        assert!(!a.insecure_tls, "default must verify certificates");
        let b = RemoteSource::new_with_options("https://h.example/g.pkg".into(), 1, true);
        assert!(b.insecure_tls, "explicit opt-in must be honoured");
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd engine && cargo test -q -p ps5upload-engine --lib insecure_tls_is_off`
Expected: FAIL — no such field or constructor.

- [ ] **Step 3: Implement**

```rust
fn build_agent(parallelism: usize, insecure_tls: bool) -> ureq::Agent {
    let keep = parallelism.max(1);
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(PIECE_TIMEOUT))
        .max_idle_connections_per_host(keep)
        .max_idle_connections(keep.saturating_mul(2).max(10))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                // Off by default. On, a machine between us and the origin can
                // substitute the package — which is why it is per-install and
                // never a global setting.
                .disable_verification(insecure_tls)
                .build(),
        )
        .build();
    ureq::Agent::new_with_config(config)
}
```

Add `pub insecure_tls: bool` to `InstallStartRequest` with `#[serde(default)]`,
and pass it into `RemoteSource::new_with_options` at the `resolve_remote_source`
call site. `RemoteSource::new` delegates with `false`.

- [ ] **Step 4: Run the test, the suite, and the Android target**

```bash
cd engine && cargo fmt --all \
  && cargo test -q -p ps5upload-engine --lib \
  && cargo check -q -p ps5upload-engine --target aarch64-linux-android
```
Expected: all pass. Android must still build — `RemotePkg` is uninhabited there.

- [ ] **Step 5: Commit**

```bash
git add engine/crates/ps5upload-engine/src/remote_pkg.rs engine/crates/ps5upload-engine/src/pkg_install.rs
git commit -m "feat(install): per-install option to skip certificate checks"
```

---

### Task 4: Client store for the per-host link-install choice

**Files:**
- Create: `client/src/state/linkInstallPrefs.ts`
- Test: `client/src/state/linkInstallPrefs.test.ts`

**Interfaces:**
- Produces: `type LinkInstallMode = "direct" | "accelerated"`;
  `useLinkInstallPrefs` with `modeFor(host): LinkInstallMode`,
  `setMode(host, mode)`, `insecureFor(host): boolean`,
  `setInsecure(host, on)`.

- [ ] **Step 1: Write the failing test**

```ts
import { beforeEach, describe, expect, it } from "vitest";
import { useLinkInstallPrefs } from "./linkInstallPrefs";

describe("link install preferences", () => {
  beforeEach(() => {
    localStorage.clear();
    useLinkInstallPrefs.setState({ modes: {}, insecure: {} });
  });

  /* A first-time user gets the mode that needs no explanation and no awake
   * computer. */
  it("defaults to direct for an unknown host", () => {
    expect(useLinkInstallPrefs.getState().modeFor("10.0.0.5")).toBe("direct");
  });

  /* The choice is per console: two PS5s can sit on different links. */
  it("remembers a choice per host", () => {
    useLinkInstallPrefs.getState().setMode("10.0.0.5", "accelerated");
    expect(useLinkInstallPrefs.getState().modeFor("10.0.0.5")).toBe("accelerated");
    expect(useLinkInstallPrefs.getState().modeFor("10.0.0.6")).toBe("direct");
  });

  /* Skipping certificate checks must never be sticky-on by accident. */
  it("defaults the certificate check to ON", () => {
    expect(useLinkInstallPrefs.getState().insecureFor("10.0.0.5")).toBe(false);
  });
});
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd client && npx vitest run src/state/linkInstallPrefs.test.ts`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement the store**

Follow `client/src/state/installSettings.ts`: zustand + `safeGetItem`/
`safeSetItem` from `../lib/safeStorage`, namespaced keys, JSON maps keyed by
host. Keys: `ps5upload.link_install_mode`, `ps5upload.link_install_insecure`.
Wrap every storage read in try/catch — the browser build runs in an insecure
context where storage can throw.

- [ ] **Step 4: Run the test**

Run: `cd client && npx vitest run src/state/linkInstallPrefs.test.ts`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add client/src/state/linkInstallPrefs.ts client/src/state/linkInstallPrefs.test.ts
git commit -m "feat(install): remember the link-install mode per console"
```

---

### Task 5: Direct mode in the install path, with a reasoned fallback

**Files:**
- Modify: `client/src/state/pkgLibrary.ts` (`installUrl`, ~line 3525)
- Test: `client/src/state/pkgLibrary.linkModes.test.ts`

**Interfaces:**
- Consumes: `useLinkInstallPrefs` (Task 4); Tauri `pkg_dpi_install` with
  `{ ps5Addr, localPs5Path, titleId, packageAppVer }` — `localPs5Path` accepts
  an http(s) URL.
- Produces: `installUrl(url, host, opts?: { mode?: LinkInstallMode })`.

- [ ] **Step 1: Write the failing test**

```ts
  /* Direct hands the URL to the console's own installer and never opens the
   * engine's proxy — that is the whole point: the PC can then sleep. */
  it("direct mode calls pkg_dpi_install with the url", async () => {
    mockedInvoke.mockResolvedValue({ ok: true, rc: 0 });
    await usePkgLibrary("10.0.0.5:9114")
      .getState()
      .installUrl("https://h.example/g.pkg", "10.0.0.5:9114", { mode: "direct" });
    const call = mockedInvoke.mock.calls.find((c) => c[0] === "pkg_dpi_install");
    expect(call).toBeTruthy();
    expect((call![1] as { localPs5Path: string }).localPs5Path).toBe(
      "https://h.example/g.pkg",
    );
  });

  /* A refusal must say why and continue, not dead-end the user. */
  it("falls back to accelerated when direct is refused", async () => {
    mockedInvoke.mockImplementation(async (cmd: string) => {
      if (cmd === "pkg_dpi_install") throw new Error("dpi daemon unreachable");
      return { total_size: 1024 };
    });
    const r = await usePkgLibrary("10.0.0.5:9114")
      .getState()
      .installUrl("https://h.example/g.pkg", "10.0.0.5:9114", { mode: "direct" });
    expect(String(r.message)).toMatch(/dpi daemon unreachable/);
  });
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd client && npx vitest run src/state/pkgLibrary.linkModes.test.ts`
Expected: FAIL — `installUrl` takes no options and never calls `pkg_dpi_install`.

- [ ] **Step 3: Implement**

In `installUrl`, after the existing URL validation, branch on the mode
(argument, else `useLinkInstallPrefs.getState().modeFor(host)`):

```ts
      if (mode === "direct") {
        try {
          await invoke("pkg_dpi_install", {
            ps5Addr: host,
            localPs5Path: trimmed,
            titleId: null,
            packageAppVer: null,
          });
          return { ok: true, message: "The PS5 is downloading and installing the package." };
        } catch (e) {
          // Name the reason and carry on rather than dead-ending: the
          // accelerated path does not depend on the DPI daemon.
          log.info("install", `direct install refused, using this computer instead: ${pkgError(e)}`);
        }
      }
      return get().installStream({ remoteUrl: trimmed }, host);
```

- [ ] **Step 4: Run the tests**

Run: `cd client && npx vitest run src/state/pkgLibrary.linkModes.test.ts && npx vitest run`
Expected: new tests PASS, existing 1,485 still pass.

- [ ] **Step 5: Commit**

```bash
git add client/src/state/pkgLibrary.ts client/src/state/pkgLibrary.linkModes.test.ts
git commit -m "feat(install): let the PS5 download a link directly"
```

---

### Task 6: The choice in the Install Package screen

**Files:**
- Modify: `client/src/screens/InstallPackage/index.tsx` (URL row, ~line 1361)
- Modify: `client/src/i18n/locales/en.ts` and all 19 other locale files
- Modify: `scripts/i18n-known-missing.json` only if a string is deliberately untranslated

**Interfaces:**
- Consumes: `useLinkInstallPrefs` (Task 4), `installUrl(url, host, opts)` (Task 5).

- [ ] **Step 1: Add the strings to en.ts**

```ts
  "pkglib.url.mode.direct": "Let the PS5 download it",
  "pkglib.url.mode.direct_hint":
    "Fastest on a good connection, and you can close this app once it starts.",
  "pkglib.url.mode.accelerated": "Download through this computer",
  "pkglib.url.mode.accelerated_hint":
    "Better on slow or distant links. Keep this computer awake until it finishes.",
  "pkglib.url.insecure": "Skip the certificate check",
  "pkglib.url.insecure_hint":
    "For your own server or a site with an out-of-date certificate. Only applies when this computer downloads it.",
```

- [ ] **Step 2: Translate into the other 19 locales**

Add the same six keys to every file in `client/src/i18n/locales/`. Locale files
are generated at column 0 — match each file's existing indentation exactly and
do not reformat.

- [ ] **Step 3: Run the coverage gate**

Run: `node scripts/i18n-coverage.mjs`
Expected: `[i18n-coverage] ok (20 languages)`

- [ ] **Step 4: Render the control**

Above the existing Install-link button:

```tsx
const mode = useLinkInstallPrefs((s) => s.modeFor(host));
const insecure = useLinkInstallPrefs((s) => s.insecureFor(host));
const setMode = useLinkInstallPrefs((s) => s.setMode);
const setInsecure = useLinkInstallPrefs((s) => s.setInsecure);
// ...
<fieldset className="mt-2 space-y-1">
  {(["direct", "accelerated"] as const).map((m) => (
    <label key={m} className="flex items-start gap-2 text-sm">
      <input
        type="radio"
        name="link-install-mode"
        checked={mode === m}
        onChange={() => setMode(host, m)}
      />
      <span>
        {tr(`pkglib.url.mode.${m}`)}
        <span className="block text-xs text-[var(--color-muted)]">
          {tr(`pkglib.url.mode.${m}_hint`)}
        </span>
      </span>
    </label>
  ))}
  <label className="flex items-start gap-2 text-sm">
    <input
      type="checkbox"
      checked={insecure && mode === "accelerated"}
      disabled={mode === "direct"}
      onChange={(e) => setInsecure(host, e.target.checked)}
    />
    <span>
      {tr("pkglib.url.insecure")}
      <span className="block text-xs text-[var(--color-muted)]">
        {tr("pkglib.url.insecure_hint")}
      </span>
    </span>
  </label>
</fieldset>
```

Pass the mode when installing: `installUrl(remoteUrl.trim(), host, { mode })`.

- [ ] **Step 5: Type-check, lint and test**

```bash
cd client && npx tsc --noEmit -p tsconfig.json && npm run lint && npx vitest run
```
Expected: clean, 1,485+ tests pass.

- [ ] **Step 6: Commit**

```bash
git add client/src/screens/InstallPackage/index.tsx client/src/i18n/locales
git commit -m "feat(install): choose how a link install downloads"
```

---

### Task 7: Changelog

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Add the entry above the previous release**

Write it for users, not maintainers: the PS5 can now fetch a link itself
(fastest, frees the computer), this computer can fetch it with many connections
(better on slow links), and certificate checks can be skipped for self-hosted
sources. State the measured 114 MB/s for direct on a local source, and do not
claim an improvement for throttled sources that has not been measured.

- [ ] **Step 2: Commit**

```bash
git add CHANGELOG.md
git commit -m "docs: changelog for link install modes"
```
