# Activity Bar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One bottom bar that shows every long-running job — uploads, installs, converts, backups, library actions and the rest — with a one-line summary and an expandable panel of rows (progress, speed, time left, Cancel/Retry, Just finished).

**Architecture:** The unified task store (`client/src/state/tasks.ts`) becomes the bar's only source. Features that don't report yet register tasks directly (or through a small `trackTask` helper); library actions are bridged from the activity log. A pure `summarize()` model turns tasks into the summary line and panel rows, and a small panel store holds open/seen state shared by the status strip and the panel.

**Tech Stack:** React 19 + TypeScript + zustand, Vitest (node env, static-markup component tests via `react-dom/server`).

**Spec:** `docs/superpowers/specs/2026-09-26-activity-bar-design.md`

## Global Constraints

- Collapsed line: `⟳ N running · <Short> <pct>% …` — at most three jobs by name, then `+N more`.
- Idle: `Nothing running`; for 5 s after a job ends: `✓ <job> finished` / `✕ <job> failed`.
- A failed job leaves a red `N failed` badge until the panel has been opened.
- Stale: a running task with no update for 60 s shows "No update for 1 min" (minutes rounded down).
- Just finished: this session's last 5 terminal tasks.
- The bar reads only `useTaskStore`; the activity log stays for the Activity screen.
- Retry and Cancel go through `taskCapabilities` / `commandTask` (`state/taskControls.ts`), never by editing tasks.
- i18n: every new string in `client/src/i18n/locales/en.ts` (column-0 lines) and its key in each locale's `missing` list in `scripts/i18n-known-missing.json`; `npm run i18n:check` passes.
- Tests run in vitest's node env: store tests stub `window.localStorage` (see `state/linkInstallPrefs.test.ts`); components are tested with `renderToStaticMarkup`.

## Review Focus

1. **Many jobs at once** (a 12-item upload queue plus an install) → the collapsed line stays one line (three names + `+N more`); the panel lists all rows and scrolls. Test in Task 2.
2. **App reload with jobs running** → reloaded tasks are `interrupted`; they appear in Just finished as interrupted, never as running, and don't raise the failed badge. Test in Task 2.
3. **A feature throws before finishing its task** → the task still ends (`failed` with the error), so no row spins forever. Test in Task 5 (`trackTask` finishes in `finally`).
4. **An upload is in both the activity log and the task store** → the library bridge mirrors only `library-*` kinds, so uploads are never counted twice. Test in Task 3.
5. **Cancel on a job whose owner already finished** (conversion done a moment ago) → nothing happens, no error; the button is hidden once `taskCapabilities` says it can't cancel. Test in Task 4.

---

## File Structure

- Modify `client/src/state/tasks.ts` — `stage` field, new task kinds.
- Create `client/src/state/activitySummary.ts` (+ test) — pure `summarize()`, `shortLabel()`, `routeForTask()`.
- Create `client/src/state/activityPanel.ts` — panel open state, seen-failed ids, session start.
- Create `client/src/state/libraryTaskBridge.ts` (+ test) — library activity → tasks; installed from `taskWiring`.
- Modify `client/src/state/fpkgConversion.ts` (+ test) — Convert/.ffpfsc tasks; `client/src/state/taskControls.ts` — `fpkg-convert` owner.
- Create `client/src/state/trackTask.ts` (+ test) — wrap a one-shot async operation in a task.
- Modify `screens/Backup/index.tsx`, `screens/Saves/index.tsx`, `screens/InstalledApps/BackportPanel.tsx`, `screens/InstalledApps/BackportPackCard.tsx`, `screens/InstalledApps/LibrarySourcePicker.tsx`, `screens/BugReport/index.tsx` — call `trackTask` / report scan progress.
- Modify `client/src/state/pkgLibrary.ts` (+ test) — Install All parent task.
- Rewrite `client/src/layout/ActivityBar.tsx` as the panel; modify `client/src/layout/StatusBar.tsx` summary slot; create `client/src/layout/ActivitySummary.tsx` (+ test).

---

### Task 1: Task stage and new kinds

**Files:** Modify `client/src/state/tasks.ts`; Test `client/src/state/tasks.test.ts` (create if absent — follow the localStorage stub pattern).

**Interfaces:**
- Produces: `Task.stage?: string`; `updateTask` patch may include `stage`; `TaskKind` gains `"fpkg-convert" | "ffpfsc-compress" | "backport-patch" | "fakelib-scan" | "fakelib-import" | "bug-report" | "install-batch" | "library-op"`.

- [ ] **Step 1: Failing test**

```ts
it("carries a stage name through updates", () => {
  const s = useTaskStore.getState();
  const id = s.registerTask({ kind: "fpkg-convert", origin: "convert", label: "Convert Minecraft", consoleId: "" });
  s.updateTask(id, { stage: "Compress", progress: { current: 5, total: 10, unit: "bytes" } });
  expect(useTaskStore.getState().getTask(id)).toMatchObject({ stage: "Compress", kind: "fpkg-convert" });
});
```

- [ ] **Step 2: Run** `cd client && npx vitest run src/state/tasks.test.ts` — Expected: FAIL (type error / `stage` undefined).
- [ ] **Step 3: Implement** — add `stage?: string;` to `Task` (doc: "The step the job is on, e.g. \"Compress\", \"Sending\"; shown in the activity panel."), add `"stage"` to the `updateTask` `Pick<…>`, and the new kinds to `TaskKind` with one-line comments.
- [ ] **Step 4: Run** the test and `npx tsc --noEmit -p .` — Expected: PASS, no type errors.
- [ ] **Step 5: Commit** `feat(tasks): a task can name its stage; kinds for the jobs the bar will show`.

### Task 2: The summary model

**Files:** Create `client/src/state/activitySummary.ts`, `client/src/state/activitySummary.test.ts`.

**Interfaces:**
- Consumes: `Task`, `isTerminal` from `./tasks`.
- Produces:

```ts
export interface ActivityRow {
  id: string;
  task: Task;
  short: string;          // "Upload", "Convert", "Install", …
  pct: number | null;     // 0..100, null when the job reports no progress
  staleMin: number | null; // minutes without an update (>= 1), else null
}
export interface FinishedRow { id: string; task: Task; outcome: "done" | "failed" | "cancelled" | "unverified" | "interrupted"; agoMs: number }
export interface ActivitySummary {
  running: ActivityRow[];          // running first by start time, then queued/awaiting/paused
  headline: { short: string; pct: number | null }[]; // at most 3
  more: number;                    // running.length - headline.length
  finished: FinishedRow[];         // this session, newest first, at most 5
  failedUnseen: number;            // failed (not interrupted) finished this session, id not in `seen`
  flash: { short: string; label: string; outcome: "done" | "failed" } | null; // a task ended < 5 s ago
}
export function shortLabel(kind: TaskKind): string;
export function routeForTask(task: Task): string;
export function summarize(tasks: Task[], opts: { now: number; sessionStart: number; seen: ReadonlySet<string> }): ActivitySummary;
```

`shortLabel`: upload-* → "Upload"; download → "Download"; fs-copy/move → "Copy"/"Move"; fs-delete → "Delete"; fs-rename → "Rename"; pkg-install/pkg-dpi-install → "Install"; install-batch → "Install all"; backup-snapshot → "Backup"; backup-restore → "Restore"; save-backup → "Save backup"; save-restore → "Save restore"; fpkg-convert → "Convert"; ffpfsc-compress → "Compress"; backport-patch → "Backport"; fakelib-scan → "Library scan"; fakelib-import → "Library import"; bug-report → "Bug report"; library-* → "Library"; cheat-download → "Cheats"; icon-fetch → "Artwork".
`routeForTask`: upload-* → `/upload`; fs-* and download → `/file-system`; pkg-* / install-batch → `/install-package`; backup-* → `/backup`; save-* → `/saves`; fpkg-convert / ffpfsc-compress → `/convert`; backport-patch / fakelib-* → `/installed`; bug-report → `/bug-report`; library-* → `/library`; cheat-download → `/cheats`; otherwise `/tasks`.
`pct` = `progress.current / progress.total * 100` when `total > 0`. Stale when `now - updatedAtMs >= 60_000` → `Math.floor(… / 60_000)`. Finished: terminal tasks with `endedAtMs >= sessionStart`. `flash`: the newest terminal task with `now - endedAtMs < 5000` and outcome done/failed.

- [ ] **Step 1: Failing tests**

```ts
import { describe, expect, it } from "vitest";
import type { Task } from "./tasks";
import { routeForTask, shortLabel, summarize } from "./activitySummary";

const NOW = 1_000_000;
let n = 0;
function task(p: Partial<Task>): Task {
  n += 1;
  return {
    id: `t${n}`, kind: "upload-file", origin: "x", createdAt: "", status: "running",
    attempts: 0, maxAttempts: 1, consoleId: "h", payload: {}, label: `Job ${n}`,
    updatedAtMs: NOW - 1000, endedAtMs: null, ...p,
  } as Task;
}
const opts = (seen: string[] = []) => ({ now: NOW, sessionStart: NOW - 3_600_000, seen: new Set(seen) });

describe("summarize", () => {
  it("names at most three running jobs and counts the rest", () => {
    const tasks = [
      ...Array.from({ length: 12 }, (_, i) => task({ kind: "upload-file", progress: { current: i, total: 12, unit: "files" } })),
      task({ kind: "pkg-dpi-install" }),
    ];
    const s = summarize(tasks, opts());
    expect(s.running).toHaveLength(13);
    expect(s.headline).toHaveLength(3);
    expect(s.more).toBe(10);
  });

  it("marks a job with no update for a minute as stale", () => {
    const s = summarize([task({ updatedAtMs: NOW - 125_000 })], opts());
    expect(s.running[0].staleMin).toBe(2);
    expect(summarize([task({})], opts()).running[0].staleMin).toBeNull();
  });

  it("lists this session's finished jobs, newest first, five at most", () => {
    const done = Array.from({ length: 7 }, (_, i) => task({ status: "done", endedAtMs: NOW - (i + 1) * 60_000 }));
    const old = task({ status: "done", endedAtMs: NOW - 7_200_000 });
    const s = summarize([...done, old], opts());
    expect(s.finished).toHaveLength(5);
    expect(s.finished[0].agoMs).toBe(60_000);
  });

  it("badges failures until seen, but not interrupted jobs from a reload", () => {
    const failed = task({ status: "failed", endedAtMs: NOW - 10_000 });
    const interrupted = task({ status: "interrupted", endedAtMs: NOW - 10_000 });
    expect(summarize([failed, interrupted], opts()).failedUnseen).toBe(1);
    expect(summarize([failed, interrupted], opts([failed.id])).failedUnseen).toBe(0);
    expect(summarize([interrupted], opts()).running).toHaveLength(0);
    expect(summarize([interrupted], opts()).finished[0].outcome).toBe("interrupted");
  });

  it("flashes a job that ended in the last five seconds", () => {
    const s = summarize([task({ kind: "backup-snapshot", status: "done", endedAtMs: NOW - 2000, label: "Backup PS5" })], opts());
    expect(s.flash).toEqual({ short: "Backup", label: "Backup PS5", outcome: "done" });
    expect(summarize([task({ status: "done", endedAtMs: NOW - 9000 })], opts()).flash).toBeNull();
  });

  it("shows no percent for a job without progress", () => {
    expect(summarize([task({ kind: "backup-snapshot" })], opts()).running[0].pct).toBeNull();
  });
});

describe("labels and routes", () => {
  it("routes each job to its screen", () => {
    expect(shortLabel("fpkg-convert")).toBe("Convert");
    expect(routeForTask(task({ kind: "fpkg-convert" }))).toBe("/convert");
    expect(routeForTask(task({ kind: "pkg-dpi-install" }))).toBe("/install-package");
    expect(routeForTask(task({ kind: "library-op" }))).toBe("/library");
  });
});
```

- [ ] **Step 2: Run** `cd client && npx vitest run src/state/activitySummary.test.ts` — Expected: FAIL (module not found).
- [ ] **Step 3: Implement** `activitySummary.ts` per the interface (≈90 lines; ordering: status `running` before others, then `createdAt`/`updatedAtMs` ascending).
- [ ] **Step 4: Run** the test — Expected: PASS.
- [ ] **Step 5: Commit** `feat(activity): summarize tasks into the bar's line and rows`.

### Task 3: Library actions reach the task store

**Files:** Create `client/src/state/libraryTaskBridge.ts`, `client/src/state/libraryTaskBridge.test.ts`; Modify `client/src/state/taskWiring.ts` (call `installLibraryTaskBridge()` at the end of `installTaskWiring`).

**Interfaces:**
- Consumes: `useActivityHistoryStore` (`entries: ActivityEntry[]`, entry `kind/label/outcome/bytes/totalBytes/error/addr`), `useTaskStore`.
- Produces: `export function installLibraryTaskBridge(): void` — idempotent; for each activity entry whose `kind` starts with `"library-"`: on first sight while `running`, `registerTask({ kind: <"library-mount"|"library-register"|"library-unregister"|"library-launch" when it matches, else "library-op">, origin: "library", label: entry.label, detail: entry.detail, consoleId: entry.addr ?? "" })`; on byte updates, `updateTask(progress bytes)`; on outcome `done` → `finishTask(done)`, `failed` → `finishTask(failed, { lastError: { code: "LIBRARY_OP_FAILED", message: entry.error ?? "failed", recoverable: false } })`, `stopped` → `finishTask(cancelled)`.

- [ ] **Step 1: Failing tests**

```ts
it("mirrors a library action into a task and ends it", () => {
  installLibraryTaskBridge();
  const a = useActivityHistoryStore.getState();
  const id = a.start("library-move", "Move Game.exfat", { addr: "10.0.0.2:9113", totalBytes: 100 });
  const t = () => useTaskStore.getState().tasks.find((x) => x.label === "Move Game.exfat");
  expect(t()).toMatchObject({ kind: "library-op", status: "running" });
  a.update(id, { bytes: 40 });
  expect(t()?.progress).toMatchObject({ current: 40, total: 100 });
  a.finish(id, "done");
  expect(t()?.status).toBe("done");
});

it("never mirrors uploads, which the task store already has", () => {
  installLibraryTaskBridge();
  const before = useTaskStore.getState().tasks.length;
  useActivityHistoryStore.getState().start("upload", "Upload A.exfat");
  expect(useTaskStore.getState().tasks.length).toBe(before);
});
```

(Use the store's real `finish` signature — check `activityHistory.ts`; stub `window.localStorage` as in `linkInstallPrefs.test.ts`.)

- [ ] **Step 2: Run** — Expected: FAIL (module not found).
- [ ] **Step 3: Implement** with a `Map<activityId, taskId>` and one `useActivityHistoryStore.subscribe` diffing entries; wire it from `installTaskWiring`.
- [ ] **Step 4: Run** the test, then `npx vitest run src/state` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(activity): library actions show in the task store`.

### Task 4: Convert and .ffpfsc report as tasks

**Files:** Modify `client/src/state/fpkgConversion.ts`, `client/src/state/fpkgConversion.test.ts`, `client/src/state/taskControls.ts`, `client/src/state/tasks.ts` (`TaskControlRef` gains `{ owner: "fpkg-convert" }`).

**Interfaces:**
- Consumes: Task 1 kinds/stage; the pipeline store's `beginRun`, `enterStage`, `fail`, `finish`, `installDone` (internal).
- Produces: each run registers one task (`kind: "fpkg-convert"` or `"ffpfsc-compress"`, `origin: "convert"`, `label: "Convert <basename>"` / `"Compress <basename>"`, `control: { owner: "fpkg-convert" }`); `enterStage` updates `stage` (the stage label: Check source / Plan package / Compress / Write package / Verify / Send to PS5 / Install on PS5) and `progress` (bytes); `fail` → `finishTask(failed, lastError)`, cancel message → `finishTask(cancelled)`; `finish`/`installDone` → `finishTask(done)`. `taskCapabilities` for owner `fpkg-convert`: `canCancel` when the pipeline is running with a `jobId`; `commandTask(cancel)` calls `useFpkgConversion.getState().cancel()`.

- [ ] **Step 1: Failing tests** (append to `fpkgConversion.test.ts`; mock `./tasks` is NOT needed — use the real store with the localStorage stub)

```ts
it("reports the run as a task through its stages to done", async () => {
  jobStatus
    .mockResolvedValueOnce({ status: "running", stage: { id: "compress", index: 2, count: 5, done: 5, total: 10 } })
    .mockResolvedValueOnce({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
  await useFpkgConversion.getState().start(req, { install: false, host: null });
  const t = () => useTaskStore.getState().tasks.find((x) => x.kind === "fpkg-convert");
  await tick();
  expect(t()).toMatchObject({ status: "running", stage: "Compress", label: "Convert a" });
  await tick();
  expect(t()?.status).toBe("done");
});

it("offers Cancel only while the build runs", async () => {
  jobStatus.mockResolvedValue({ status: "done", dest: "/out/a.pkg", bytes_sent: 1 });
  await useFpkgConversion.getState().start(req, { install: false, host: null });
  const t = () => useTaskStore.getState().tasks.find((x) => x.kind === "fpkg-convert")!;
  expect(taskCapabilities(t()).canCancel).toBe(true);
  await tick();
  expect(taskCapabilities(t()).canCancel).toBe(false);
  expect(await commandTask(t(), "cancel")).toBe(true); // no-op, no throw
});
```

- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement** (store the task id in the `running` pipeline as `taskId: string | null`; a `STAGE_LABEL` map mirrors RunCard's labels).
- [ ] **Step 4: Run** `npx vitest run src/state` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(activity): Convert and .ffpfsc runs show in the bar`.

### Task 5: `trackTask` and the one-shot features

**Files:** Create `client/src/state/trackTask.ts`, `client/src/state/trackTask.test.ts`; Modify `screens/Backup/index.tsx:130,166`, `screens/Saves/index.tsx:193,274,384,560`, `screens/InstalledApps/BackportPanel.tsx:176`, `screens/InstalledApps/BackportPackCard.tsx:151`, `screens/BugReport/index.tsx:338`.

**Interfaces:**
- Produces:

```ts
/** Run `op` as a task in the activity bar: registered running, finished done on success,
 *  failed with the error's message when it throws (and rethrown). */
export async function trackTask<T>(
  init: { kind: TaskKind; origin: string; label: string; consoleId?: string; detail?: string },
  op: (report: (patch: { stage?: string; progress?: TaskProgress }) => void) => Promise<T>,
): Promise<T>;
```

- [ ] **Step 1: Failing tests**

```ts
it("ends the task done when the operation succeeds", async () => {
  await trackTask({ kind: "backup-snapshot", origin: "backup", label: "Backup PS5" }, async (report) => {
    report({ stage: "Copying" });
    return 1;
  });
  const t = useTaskStore.getState().tasks.find((x) => x.label === "Backup PS5");
  expect(t).toMatchObject({ status: "done", stage: "Copying" });
});

it("ends the task failed, never running, when the operation throws", async () => {
  await expect(
    trackTask({ kind: "save-restore", origin: "saves", label: "Restore save" }, async () => {
      throw new Error("USB gone");
    }),
  ).rejects.toThrow("USB gone");
  const t = useTaskStore.getState().tasks.find((x) => x.label === "Restore save");
  expect(t).toMatchObject({ status: "failed", lastError: { message: "USB gone" } });
});
```

- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement** `trackTask` (register → try `await op(report)` → `finishTask(done)`; catch → `finishTask(failed, { lastError: { code: "OP_FAILED", message, recoverable: false } })` and rethrow). Then wrap each call site, e.g. Backup: `const result = await trackTask({ kind: "backup-snapshot", origin: "backup", label: \`Backup ${tag.trim()}\`, consoleId: addr }, () => backupSnapshot(tag.trim(), path.trim(), addr));`. Saves: `save-backup` / `save-restore` with the title id in the label. Backport patch: `backport-patch`, `label: \`Backport ${titleId}\``. Pack import: `fakelib-import`. Bug report: `bug-report`, `label: "Bug report"`.
- [ ] **Step 4: Library scan progress** — in `LibrarySourcePicker.tsx`, register a `fakelib-scan` task when the scan starts; in the poll loop at line 148 call `updateTask(id, { progress: { current: snapshot.titlesDone, total: snapshot.titlesTotal, unit: "items" }, stage: snapshot.current })`; finish done when `snapshot.done`, failed on error.
- [ ] **Step 5: Run** `npx vitest run src/state src/screens && npx tsc --noEmit -p .` — Expected: PASS.
- [ ] **Step 6: Commit** `feat(activity): backups, saves, backport and bug reports show in the bar`.

### Task 6: Install All as one parent row

**Files:** Modify `client/src/state/pkgLibrary.ts` (`installAll`, ~line 3525); Test `client/src/state/pkgLibrary.test.ts`.

**Interfaces:**
- Produces: `installAll` registers `{ kind: "install-batch", origin: "pkg.install-all", label: "Install all (<n>)", consoleId: host }`, updates `stage: "Installing <i> of <n>"` and `progress { current: i-1, total: n, unit: "items" }` before each item, finishes done when all succeeded, failed (message "k of n failed") otherwise.

- [ ] **Step 1: Failing test** — drive `installAll` with two staged rows whose installs resolve (reuse the file's existing `installAll` test setup); assert one `install-batch` task that ends `done` with `progress.total === 2`.
- [ ] **Step 2: Run** — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `npx vitest run src/state/pkgLibrary.test.ts` — Expected: PASS.
- [ ] **Step 5: Commit** `feat(activity): Install All shows as one row with its progress`.

### Task 7: The bar — summary line and panel

**Files:** Create `client/src/state/activityPanel.ts`, `client/src/layout/ActivitySummary.tsx`, `client/src/layout/ActivitySummary.test.tsx`; Rewrite `client/src/layout/ActivityBar.tsx`; Modify `client/src/layout/StatusBar.tsx` (replace the `runningCount` slot, lines ~52-56 and ~166-179); `en.ts`, `scripts/i18n-known-missing.json`.

**Interfaces:**
- Consumes: `summarize`, `routeForTask`, `shortLabel` (Task 2); `taskCapabilities`, `commandTask`; `useTaskStore`.
- Produces:
  - `useActivityPanel` zustand store: `{ open: boolean; seen: Set<string>; sessionStart: number; toggle(): void; markSeen(ids: string[]): void }` — opening marks every finished failed id as seen.
  - `ActivitySummaryLine({ summary, onToggle, open })` — the strip's slot: spinner + `N running · Short pct% …` (+`+N more`), or the flash, or `Nothing running`; red `N failed` badge when `failedUnseen > 0`.
  - `ActivityPanelView({ summary, now, onOpen, onCancel, onRetry, canCancel, canRetry })` — running rows (short, label, stage, bar, speed from `task.rate`, time left from `task.eta`, stale "No update for N min", Cancel when `canCancel(task)`), Just finished rows (✓/✕/⚠, label, ago, one-line reason, Retry when `canRetry(task)`), **See all activity** link.
  - `ActivityBar` (connected): renders `ActivityPanelView` above the strip when `open`; ticks `now` once a second while open.

- [ ] **Step 1: Failing render tests** (static markup, `useTr` mocked as in `FpkgConvert/RunCard.test.tsx`)

```tsx
it("summarises running jobs on one line", () => {
  const out = renderToStaticMarkup(<ActivitySummaryLine summary={summaryWith(4)} open={false} onToggle={() => {}} />);
  expect(out).toContain("4 running");
  expect(out).toContain("+1 more");
});
it("says nothing is running, and badges unseen failures", () => {
  const out = renderToStaticMarkup(<ActivitySummaryLine summary={{ ...empty, failedUnseen: 2 }} open={false} onToggle={() => {}} />);
  expect(out).toContain("Nothing running");
  expect(out).toContain("2 failed");
});
it("shows Cancel only where the job can be cancelled, and the stale note", () => {
  const out = renderToStaticMarkup(
    <ActivityPanelView summary={summaryWithRows()} now={NOW} onOpen={noop} onCancel={noop} onRetry={noop}
      canCancel={(t) => t.id === "a"} canRetry={() => false} />,
  );
  expect((out.match(/Cancel/g) ?? []).length).toBe(1);
  expect(out).toContain("No update for 2 min");
  expect(out).toContain("See all activity");
});
```

(`summaryWith`, `summaryWithRows`, `empty` are small fixture builders in the test file producing `ActivitySummary` values.)

- [ ] **Step 2: Run** — Expected: FAIL (modules not found).
- [ ] **Step 3: Implement** the store, the two views, the connected `ActivityBar`, and the `StatusBar` slot (it renders `ActivitySummaryLine` bound to `useActivityPanel` and a `summarize` result selected with `useShallow` on the fields the line needs). Remove the old activity-log-based running count from `StatusBar` and the old `ActivityBar` body. Add i18n keys.
- [ ] **Step 4: Run** `npx vitest run && npx tsc --noEmit -p . && npx eslint src/layout src/state && cd .. && npm run i18n:check` — Expected: all pass.
- [ ] **Step 5: Commit** `feat(ui): one activity bar for every long-running job`.

### Task 8: End to end

- [ ] **Step 1: Full gate** — `cd client && npx vitest run && npx tsc --noEmit -p . && cd .. && npm run i18n:check`. Expected: green.
- [ ] **Step 2: In the app** (user): start a Convert of Minecraft and an upload together; the strip reads `2 running · Convert …% · Upload …%`; the panel shows both rows with stage, bar, speed and time left; Cancel the upload; open a row → its screen; after the convert ends, `✓ Convert … finished` flashes and the job sits in Just finished.
- [ ] **Step 3: Commit fixes**, if any.
