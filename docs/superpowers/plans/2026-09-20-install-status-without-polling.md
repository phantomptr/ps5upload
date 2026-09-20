# Install Status Without Polling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the payload calling `sceAppInstUtilGetInstallStatus` (which kills the process that calls it), and stop the resulting unverified-but-accepted install being reported to the user as a failure.

**Architecture:** One payload change routes the last polling tier through the synthetic-DONE bypass the other three tiers already use. Two client changes then stop that state being rendered as an error and keep verifying in the background, reusing the progress-based wait the DPI install path already uses successfully.

**Tech Stack:** C (PS5 payload SDK), TypeScript/React (client), Rust (engine — read only, no changes).

**Spec:** `docs/superpowers/specs/2026-09-20-install-status-without-polling-design.md`

## Global Constraints

- **Branch:** `install-status-no-poll`, which already exists and holds the spec commit.
- **`sceAppInstUtilGetInstallStatus` must not be called from anywhere in `payload/`.** It is hardware-proven (two consoles, A/B, `payload/dpi/ezremote_dpi.c:257`) to kill whatever process calls it, in-process and cross-process alike.
- **No `fork()` and no cross-mount `rename()`** anywhere in payload code.
- **Payload JSON keys are `snake_case`** — a camelCase key is silently dropped by serde on the engine side.
- **Never reformat the i18n locale files.** They are generated at column 0; reformatting breaks the duplicate-key gate. (No locale change is expected in this plan — `pkgLibrary.ts` has no i18n import.)
- **`client/src/i18n/locales/en.ts` is the source of truth.** New keys must be translated in every locale or added to that locale's `missing` array in `scripts/i18n-known-missing.json`, or the coverage gate fails CI. Prefer the allowlist over machine translation.
- **No `BigInt`** anywhere in client code — the build target cannot down-level it.
- `make payload`, `make test-payload`, `make test-client`, `make lint-client` and `node scripts/i18n-coverage.mjs` must all pass.
- End every commit message with: `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`

---

### Task 1: Remove the last `GetInstallStatus` poll from the payload

**Files:**
- Modify: `payload/src/bgft.c` (the `APPINST_VIA_TIER0_FLAG` comment block ~line 210; `appinst_install_status` ~line 812; the extern declaration ~line 144; the dispatch in `bgft_install_status` ~line 1828)

**Interfaces:**
- Consumes: nothing.
- Produces: `bgft_install_status` returns `BGFT_PHASE_DONE, downloaded=0, total=0, err=0` for every AppInstUtil task id. No later task depends on new symbols.

- [ ] **Step 1: Confirm the current call site exists**

Run:
```bash
grep -n "sceAppInstUtilGetInstallStatus" payload/src/bgft.c
```
Expected: an `extern` declaration around line 144, and exactly one real call inside `appinst_install_status` around line 866. If the call is already gone, stop and report — the task is done.

- [ ] **Step 2: Route the remaining tier to the synthetic-DONE bypass**

In `payload/src/bgft.c`, inside `bgft_install_status`, find this dispatch (it sits after the `APPINST_VIA_LOCAL_FLAG` block):

```c
    if ((task_id & APPINST_TASK_ID_FLAG) != 0) {
        return appinst_install_status(task_id, out_phase, out_downloaded,
                                       out_total, out_err_code);
    }
```

Replace it with:

```c
    /* Every AppInstUtil tier now takes the synthetic-DONE bypass, including
     * plain InstallByPackage. Sony's sceAppInstUtilGetInstallStatus kills the
     * process that calls it — measured 2026-09-12 on Pro FW 9.60 and Phat FW
     * 5.10, A/B against an otherwise identical build, 2/2 dead with the poll
     * and 1/1 alive without it. See the block comment in
     * payload/dpi/ezremote_dpi.c, which also retracts the older theory that
     * only a CROSS-process poller is affected: it dies in-process too.
     *
     * The install itself is unaffected — InstallByPackage already returned 0,
     * and Sony finishes it in the background. Proving completion is the
     * HOST's job: the engine re-verifies the installed artifact (category,
     * size, fingerprint) and watches APP_VER move for patches. Neither can
     * crash the console. */
    if ((task_id & APPINST_TASK_ID_FLAG) != 0) {
        /* Free the slot now — this response is terminal for the engine, so
         * holding the slot only leaks one of the 16 and eventually yields
         * TASK_TABLE_FULL. Release is idempotent (it strips the flags to find
         * the index). Same reasoning as the two bypasses above. */
        appinst_task_release(task_id);
        *out_phase = BGFT_PHASE_DONE;
        *out_downloaded = 0;
        *out_total = 0;
        *out_err_code = 0;
        return 0;
    }
```

- [ ] **Step 3: Delete the now-unreachable poller and its extern**

Delete the whole `appinst_install_status` function (it begins with the comment
`/** Poll an in-flight AppInstUtil install.` around line 811 and ends at its
closing brace). Then delete the extern declaration:

```c
extern int sceAppInstUtilGetInstallStatus(const char *content_id,
                                          AppInstStatus *out);
```

Removing the extern is the point of this step — it makes the call impossible to
reintroduce without someone re-adding the declaration and reading why it is
gone.

If `AppInstStatus` and `AppInstStatusErrorInfo` become unused after this,
leave the type definitions in place with a one-line comment saying they
document Sony's ABI and are intentionally unused; do not delete them.

- [ ] **Step 4: Reconcile the contradictory comment**

Find the `APPINST_VIA_TIER0_FLAG` / `APPINST_VIA_LOCAL_FLAG` comment block near
line 210. It currently contains this claim:

> on firmwares where InstallByPackage succeeds, GetInstallStatus is known-safe
> and gives real download/install progress, so we must NOT blanket-bypass it —
> only the local-disk variant gets the bypass.

Replace that sentence with:

```
 * NOTE (2026-09-20): the claim that once stood here — that GetInstallStatus is
 * "known-safe" on firmwares where InstallByPackage succeeds — was never
 * measured, and the A/B in payload/dpi/ezremote_dpi.c disproves it. Every tier
 * now bypasses. These VIA_* flags no longer select between polling and not
 * polling; they are retained because they still identify which backend issued
 * a task id in logs and bug reports.
```

- [ ] **Step 5: Verify no call site remains**

Run:
```bash
grep -rn "sceAppInstUtilGetInstallStatus" payload/
```
Expected: matches ONLY inside comments (`bgft.c`'s notes, `ezremote_dpi.c`'s block comment, `sony_api_lock.h`, `authid.h`, and the unused declaration in `payload/dpi/sceAppInstUtil.h`). No line may be a call or an `extern` in `payload/src/`.

- [ ] **Step 6: Build and test**

Run: `make payload && make test-payload`
Expected: clean build under `-Werror` (in particular, no "defined but not used" for the deleted function), all selftests pass.

- [ ] **Step 7: Commit**

```bash
git add payload/src/bgft.c
git commit -m "fix(payload): never poll sceAppInstUtilGetInstallStatus

It kills the process that calls it — measured on Pro 9.60 and Phat 5.10, A/B
against an identical build, 2/2 dead with the poll and 1/1 alive without it
(see payload/dpi/ezremote_dpi.c). Three of the four AppInstUtil tiers already
bypassed it; plain InstallByPackage still polled it roughly once a second
during an install, on the strength of a 'known-safe' comment that the same
A/B disproves. That is the likely cause of the repeated helper crashes users
report when installing a PKG.

The extern is deleted too, so the call cannot be reintroduced without
re-declaring it and reading why it is gone.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 2: Stop rendering an unverified install as a failure

**Files:**
- Modify: `client/src/state/pkgLibrary.ts` (the result branch ~line 2233-2262, and `showInstallFailureToast` ~line 2280)
- Modify: `client/src/i18n/locales/en.ts`
- Modify: `scripts/i18n-known-missing.json`
- Test: `client/src/state/pkgLibrary.installOutcome.test.ts` (create)

**Interfaces:**
- Consumes: the existing `InstallOutcome` fields `installed`, `acceptedUnverified`, `stalled`, `errMessage`; the task store's `updateTask(id, patch)` and `finishTask(id, status, extras)`.
- Produces: `export function installOutcomeKind(r: { installed: boolean; acceptedUnverified?: boolean; stalled?: boolean }): "done" | "unverified" | "stalled" | "failed"` — Task 3 consumes this.

- [ ] **Step 1: Write the failing test**

Create `client/src/state/pkgLibrary.installOutcome.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { installOutcomeKind } from "./pkgLibrary";

/* An install the PS5 accepted but whose completion we could not confirm is NOT
 * a failure. PS5 installs are async and the app DB lags the filesystem both
 * ways, so historically every "install failed" of this shape was a successful
 * install that simply had not registered yet. Rendering it red taught users to
 * distrust real failures. */
describe("installOutcomeKind", () => {
  it("treats a confirmed install as done", () => {
    expect(installOutcomeKind({ installed: true })).toBe("done");
  });

  it("treats accepted-but-unverified as its own kind, not a failure", () => {
    expect(
      installOutcomeKind({ installed: false, acceptedUnverified: true }),
    ).toBe("unverified");
  });

  it("still treats a stall as a stall", () => {
    expect(installOutcomeKind({ installed: false, stalled: true })).toBe(
      "stalled",
    );
  });

  it("treats everything else as a failure", () => {
    expect(installOutcomeKind({ installed: false })).toBe("failed");
  });

  it("prefers done over unverified when both are set", () => {
    expect(
      installOutcomeKind({ installed: true, acceptedUnverified: true }),
    ).toBe("done");
  });
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd client && npx vitest run src/state/pkgLibrary.installOutcome.test.ts`
Expected: FAIL — `installOutcomeKind` is not exported.

- [ ] **Step 3: Add the classifier**

In `client/src/state/pkgLibrary.ts`, near the other exported helpers, add:

```ts
/** Which of the four install outcomes a result represents.
 *
 *  Pure so the mapping is testable without a console. The distinction that
 *  matters is `unverified` vs `failed`: the PS5 accepted the install and is
 *  very likely still working on it, so it must not be rendered as an error.
 *  See docs/superpowers/specs/2026-09-20-install-status-without-polling-design.md
 */
export function installOutcomeKind(r: {
  installed: boolean;
  acceptedUnverified?: boolean;
  stalled?: boolean;
}): "done" | "unverified" | "stalled" | "failed" {
  if (r.installed) return "done";
  if (r.acceptedUnverified) return "unverified";
  if (r.stalled) return "stalled";
  return "failed";
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cd client && npx vitest run src/state/pkgLibrary.installOutcome.test.ts`
Expected: PASS, 5 tests.

- [ ] **Step 5: Use it at the result branch**

In `client/src/state/pkgLibrary.ts`, replace the `if (result.installed) { … } else { … }` block (the one that builds `code` from `INSTALL_UNVERIFIED`/`INSTALL_STALLED`/`INSTALL_FAILED` and calls `finishTask(taskId, "failed", …)`) with:

```ts
    const kind = installOutcomeKind(result);
    if (kind === "done") {
      useTaskStore.getState().finishTask(taskId, "done", {
        progress: latestProgress
          ? { ...latestProgress, current: latestProgress.total }
          : undefined,
        detail: localPs5Path,
      });
    } else if (kind === "unverified") {
      // NOT terminal and NOT failed: the PS5 took the install and is probably
      // still writing it. `awaiting` keeps the row live so the background
      // re-verify (see scheduleInstallReverify) can flip it to done.
      useTaskStore.getState().updateTask(taskId, {
        status: "awaiting",
        progress: latestProgress,
        detail: localPs5Path,
        lastError: undefined,
      });
      showInstallUnverifiedToast(name);
    } else {
      useTaskStore.getState().finishTask(taskId, "failed", {
        progress: latestProgress,
        detail: localPs5Path,
        lastError: {
          code: kind === "stalled" ? "INSTALL_STALLED" : "INSTALL_FAILED",
          message: result.errMessage || "Install was not confirmed.",
          recoverable: true,
        },
      });
      showInstallFailureToast(
        name,
        result.errMessage || "Install was not confirmed.",
      );
    }
    return result;
```

- [ ] **Step 6: Add the informational toast**

In the same file, next to `showInstallFailureToast`, add:

```ts
/** The PS5 accepted the install but we could not confirm completion yet.
 *  Informational, never critical — a large install routinely outlives the
 *  engine's grace window while the console is still copying files.
 *
 *  Plain English, no `tr()`: this module has no i18n import and its sibling
 *  `showInstallFailureToast` is untranslated too. Adding a translator here
 *  would introduce a new i18n mechanism into a file that has none, which is
 *  out of scope. */
function showInstallUnverifiedToast(name: string): void {
  useToastStore.getState().push({
    tone: "info",
    message: `${name} is still finishing on the PS5. Large games keep installing for a while after the transfer ends — ps5upload keeps checking, and the staged package is kept until it is confirmed.`,
    action: {
      label: "Open Tasks",
      onClick: () => {
        window.history.pushState({}, "", "/tasks");
        window.dispatchEvent(new PopStateEvent("popstate"));
      },
    },
  });
}
```

No `en.ts` or allowlist changes are needed for this task — verified: there is
no i18n import in `pkgLibrary.ts`.

- [ ] **Step 7: Run the gates**

Run:
```bash
make test-client
make lint-client
```
Expected: both pass.

- [ ] **Step 8: Commit**

```bash
git add client/src/state/pkgLibrary.ts client/src/state/pkgLibrary.installOutcome.test.ts
git commit -m "fix(client): an unverified install is not a failure

The PS5 accepts an install and keeps writing it long after the engine's 180s
grace window; a 39.6 GiB game routinely outlives it. We were marking the task
failed and pushing a critical toast reading 'was not verified as installed'
for installs that went on to succeed, which teaches users to ignore real
failures.

Unverified is now its own outcome: the row stays live as 'awaiting' and the
toast is informational. Stalls and real failures are unchanged.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 3: Keep verifying in the background until the title registers

**Files:**
- Modify: `client/src/state/pkgLibrary.ts` (add `scheduleInstallReverify`, call it from the `unverified` branch added in Task 2)
- Test: `client/src/state/pkgLibrary.reverify.test.ts` (create)

**Interfaces:**
- Consumes: `installOutcomeKind` (Task 2); the existing `verifyDpiInstalledArtifact(host, contentId, packageType, expected, onStatus?)` at `pkgLibrary.ts:1411`, which already waits on artifact GROWTH rather than a fixed clock (`DPI_VERIFY_IDLE_MS` = 3 min of stillness, `DPI_VERIFY_MAX_MS` = 4 h ceiling).
- Produces: `export function installReverifyDelaysMs(attempt: number): number` — the backoff schedule, unit-tested.

- [ ] **Step 1: Write the failing test**

Create `client/src/state/pkgLibrary.reverify.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { installReverifyDelaysMs } from "./pkgLibrary";

/* The re-verify must be cheap and must not give up early. A 100 GiB install on
 * a slow internal drive is the case this exists for: the engine's grace window
 * ends in minutes, the console keeps writing for far longer. */
describe("installReverifyDelaysMs", () => {
  it("starts quickly", () => {
    expect(installReverifyDelaysMs(0)).toBe(30_000);
  });

  it("backs off but stays bounded", () => {
    expect(installReverifyDelaysMs(1)).toBe(60_000);
    expect(installReverifyDelaysMs(2)).toBe(120_000);
    expect(installReverifyDelaysMs(3)).toBe(300_000);
  });

  it("caps at five minutes however many attempts have passed", () => {
    expect(installReverifyDelaysMs(4)).toBe(300_000);
    expect(installReverifyDelaysMs(99)).toBe(300_000);
  });

  it("never returns a non-positive delay", () => {
    for (let i = 0; i < 50; i++) {
      expect(installReverifyDelaysMs(i)).toBeGreaterThan(0);
    }
  });
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd client && npx vitest run src/state/pkgLibrary.reverify.test.ts`
Expected: FAIL — `installReverifyDelaysMs` is not exported.

- [ ] **Step 3: Add the backoff schedule**

In `client/src/state/pkgLibrary.ts`:

```ts
/** Delay before re-verify attempt `attempt` (0-based).
 *
 *  Pure so the schedule is testable without timers. Quick at first because a
 *  small package often registers within a minute, then backing off to a
 *  five-minute floor so a multi-hour install costs only a handful of probes.
 */
export function installReverifyDelaysMs(attempt: number): number {
  const schedule = [30_000, 60_000, 120_000, 300_000];
  return schedule[Math.min(attempt, schedule.length - 1)];
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cd client && npx vitest run src/state/pkgLibrary.reverify.test.ts`
Expected: PASS, 4 tests.

- [ ] **Step 5: Add the re-verify driver**

In `client/src/state/pkgLibrary.ts`, add:

```ts
/** Keep asking whether an accepted-but-unverified install has registered.
 *
 *  Reuses verifyDpiInstalledArtifact, which already waits on the artifact
 *  GROWING rather than on a clock — its own comment records why a flat
 *  three-minute cap was wrong: it made success size-dependent, so a large
 *  package showed an error for an install the PS5 went on to complete.
 *
 *  Fire-and-forget. Never throws: a failure to verify leaves the row exactly
 *  as it was, which is the honest outcome.
 */
export function scheduleInstallReverify(args: {
  taskId: string;
  host: string;
  name: string;
  contentId: string | null;
  packageType: string;
  expected: PkgExpectedIdentity | undefined;
}): void {
  let attempt = 0;
  const tick = async () => {
    // Stop if the user resolved the row by hand, or the app moved on.
    const task = useTaskStore.getState().tasks.find((t) => t.id === args.taskId);
    if (!task || task.status !== "awaiting") return;
    let ok = false;
    try {
      ok = await verifyDpiInstalledArtifact(
        args.host,
        args.contentId,
        args.packageType,
        args.expected,
      );
    } catch {
      ok = false;
    }
    if (ok) {
      useTaskStore.getState().finishTask(args.taskId, "done");
      return;
    }
    const delay = installReverifyDelaysMs(attempt);
    attempt += 1;
    setTimeout(() => void tick(), delay);
  };
  setTimeout(() => void tick(), installReverifyDelaysMs(0));
}
```

- [ ] **Step 6: Call it from the unverified branch**

In the `else if (kind === "unverified")` branch added in Task 2, after
`showInstallUnverifiedToast(name);`, add:

```ts
      scheduleInstallReverify({
        taskId,
        host,
        name,
        contentId,
        packageType,
        expected,
      });
```

Use whatever the surrounding function already calls these values — read the
enclosing function signature and local variables first and pass the real ones.
If `contentId`, `packageType` or `expected` are not in scope there, derive them
the same way the nearby `verifyDpiInstalledArtifact` call site does. Do not
invent new parameters for the enclosing function.

- [ ] **Step 7: Run the gates**

Run:
```bash
make test-client
make lint-client
```
Expected: both pass. In particular there must be no unused-variable lint error
from Step 6.

- [ ] **Step 8: Commit**

```bash
git add client/src/state/pkgLibrary.ts client/src/state/pkgLibrary.reverify.test.ts
git commit -m "feat(client): keep verifying an accepted install in the background

The engine stops polling after its grace window, but the console keeps
installing. Rather than leaving the row unresolved, re-check on a backoff
(30s, 1m, 2m, then every 5m) using the same growth-based verify the DPI path
already uses, and flip the row to done the moment the title registers.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

### Task 4: Hardware verification

**Files:** none — manual verification on real consoles.

**Interfaces:**
- Consumes: Tasks 1-3.
- Produces: a verification section appended to the spec.

- [ ] **Step 1: Deploy**

Run: `make payload && make send-payload PS5_HOST=<pro-ip>`
Repeat for the Phat. Both consoles must run the build from this branch.

- [ ] **Step 2: The headline test — a large staged install**

Install a PKG of 30 GiB or more by the staged path on the Pro (FW 9.60).
Expected: the helper stays up for the entire install. Confirm from the client
that `:9114` never drops, and that the log shows no `helper went DOWN`.
This is the reported crash; its absence is the result that matters.

- [ ] **Step 3: Confirm the poll is really gone**

Fetch `/data/ps5upload/stderr.log` from the console.
Expected: no `GetInstallStatus` line anywhere in it.

- [ ] **Step 4: The 180-second cliff**

During the same install, watch the task row at and past the 3-minute mark.
Expected: it must NOT go red. It reads as still finishing, and flips to done
when the title registers — which for a 30 GiB install will be well past three
minutes.

- [ ] **Step 5: DLC and patch**

Repeat steps 2-4 with a DLC package and with a game patch. These exercise the
shared-`content_id` case: a patch and its base share an id, so an early
verification can see the already-installed base and wrongly call it done.
Expected: the patch row only goes done once the patch itself is applied
(`APP_VER` moves), not immediately.

- [ ] **Step 6: Repeat on the Phat (FW 5.10)**

The crash was measured on both consoles, so the fix must be confirmed on both.

- [ ] **Step 7: Record the result**

Append a "Hardware verification" section to
`docs/superpowers/specs/2026-09-20-install-status-without-polling-design.md`
recording, per console and firmware, which steps passed. State plainly any step
that could not be exercised rather than implying it passed.

```bash
git add docs/superpowers/specs/2026-09-20-install-status-without-polling-design.md
git commit -m "docs: hardware verification for install status without polling

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Self-review notes

**Spec coverage.** Spec §1 (delete the poll) → Task 1. §2 (keep verifying after
the spinner stops) → Task 3. §3 (stop calling it a failure) → Task 2. Spec's
"Testing" host section → the unit tests in Tasks 2 and 3; its hardware section
→ Task 4. The spec's "deliberately does not do" list adds no tasks by design.

**Deviation from the spec, deliberate.** The spec left open where the re-check
lives; this plan puts it client-side (Task 3) rather than in the engine. The
engine would have to hold a session open for the duration, and session GC keys
on activity in a way that has bitten us before; the client already owns the
task row and has a growth-based verifier to reuse. Same outcome, smaller blast
radius.

**Status vocabulary.** Task 2 reuses the existing `"awaiting"` TaskStatus rather
than adding a new one. It is already in the union, already non-terminal, already
counted by `isActivatable`, and currently unused in production code. The tradeoff:
an `awaiting` row becomes `interrupted` on app reload, which is honest — the
re-verify timer does not survive a restart either.

**Type consistency.** `installOutcomeKind` (Tasks 2, 3),
`installReverifyDelaysMs` (Task 3), `scheduleInstallReverify` (Task 3),
`verifyDpiInstalledArtifact` (existing, Task 3), `showInstallUnverifiedToast`
(Task 2) — each spelled identically everywhere it appears. Task 3 depends on
Task 2; Tasks 2 and 3 do not depend on Task 1 and could be done in either order,
but Task 1 should land first because it is what makes the unverified path
common.
