# Activity bar — one place for everything that takes time

Date: 2026-09-26. Sub-project 2 of 3 (after the Convert redesign, before remote sources).

## Goal

The bottom bar says "No active transfers" while a conversion, an install or a backup runs, because
it only knows about uploads, file operations, downloads and some library actions. The user asked
for a bar that shows every long-running job — upload, install, convert, and the rest — with its
progress, and for every feature to be checked so none is left out.

## Shape (chosen: bar + panel)

**Collapsed** — the status strip's running-count slot becomes the activity summary:

- Running: `⟳ 3 running · Upload 62% · Convert 58% · Install 30%` — at most three jobs by name,
  then `+N more`.
- Nothing running: `Nothing running`; for 5 s after a job ends, `✓ <job> finished` (or `✕ <job>
  failed`).
- A failed job leaves a red `N failed` badge until the panel has been opened.
- The console dots, firmware, keep-awake indicator and screenshot button stay as they are.
- Clicking the summary opens the panel.

**Expanded** — a panel above the strip:

- One row per running job: icon and name (plus the console it targets when there is more than
  one), its stage (e.g. "Compress", "Sending"), a progress bar, speed, time left, and **Cancel**
  where the job can be cancelled. Clicking a row opens the job's screen.
- A job that has sent no update for 60 s shows "No update for 1 min" instead of a frozen figure.
- **Just finished**: this session's last 5 finished jobs with outcome (✓/✕), how long ago, and for a
  failure its reason in one line and **Retry** where the job supports it.
- **See all activity** links to the Activity screen.

## Source of truth

The bar reads only the unified task store (`client/src/state/tasks.ts`). It already holds
uploads, the upload queue, file operations and downloads (mirrored by `state/taskWiring.ts`), and
package installs and link downloads (registered by `state/pkgLibrary.ts`). The activity log
(`state/activityHistory.ts`) stays as the Activity screen's history; the bar stops depending on it.

### Features that start reporting

Checked against the code on 2026-09-26 — none of these registers a task today:

| Feature | Where it runs | Task kind |
|---|---|---|
| Convert to FPKG, and Compress to .ffpfsc | `state/fpkgConversion.ts` | new `fpkg-convert`, `ffpfsc-compress` |
| Backup snapshot / restore | `screens/Backup` (`backupSnapshot`, `backupRestore`) | existing `backup-snapshot`, `backup-restore` |
| Save backup / restore (USB and archive) | `screens/Saves` | existing `save-backup`, `save-restore` |
| Backport SDK patch | `screens/InstalledApps/BackportPanel.tsx` (`sdkPatch`) | new `backport-patch` |
| Backport library scan / pack import | `state/fakelibCorpus.ts` (`startFakelibScan`, `importBackportPack`) | new `fakelib-scan`, `fakelib-import` |
| Library mount / register / unregister / launch / move / delete | `screens/Library` (activity log only today) | existing `library-*` kinds, bridged from the activity log |
| Install All (a batch of installs) | `state/pkgLibrary.ts` `installAll` | one parent row, "Installing 2 of 5", its installs listed under it |
| Bug report bundle | `screens/BugReport` | new `bug-report` |

Each reports progress where it has it (bytes, files, or steps) and a stage name where it has one;
where it has none, its row shows an indeterminate bar. A job with a cancel path in its feature
store gets a task control; nothing else shows Cancel.

Uploads and file operations appear in both the activity log and the task store already; the
bridge for library actions covers only the `library-*` kinds, so nothing is counted twice.

## Rules

- Rows sort running first by start time, then queued; failed rows stay in Just finished.
- Progress updates re-render the rows only, not the app (the strip subscribes to a derived
  summary, as today's bar does).
- Retry and Cancel go through each feature's own store (the task control mechanism already in
  `state/taskControls.ts`), never by editing the task.

## Testing

- A pure model (`summarize(tasks, now)`) produces the collapsed line and the panel rows: ordering,
  the three-name limit and `+N more`, stale detection at 60 s, the failed badge, the finished list
  (last 5, this session), the 5 s finished flash.
- Each newly connected feature: a test that it registers its task, reports progress, and ends it as
  done or failed (and cancelled where it can be).
- The strip and panel render tests (static markup, as the Convert cards): summary text, badge,
  rows with bar/speed/time, Cancel only where supported.

## Out of scope

Remote sources (sub-project 3); a history beyond this session (the Activity screen keeps that).
