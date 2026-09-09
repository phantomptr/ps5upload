# Backport a game, from ps5upload

**Status:** design approved 2026-09-08. Every fact below was measured on a
live FW 9.60 console during the investigation, not taken from documentation.

## The problem

A PS5 game built for newer firmware will not launch on an older console. Making
it run — "backporting" — takes four separate things, and today ps5upload does
one and a half of them across two screens that each look broken on their own.

The community workflow is worse than it sounds. Almost every failure in the
support threads is not a technical one: people copy the files and then forget,
or cannot work out how, to run the `backpork.elf` payload that makes those
files do anything. One asked how to autoload it, one could not find it in the
etaHEN toolbox, one gave up and updated to firmware 12.00 instead.

## What a backport actually is

Measured across 13 backported titles and one raw dump we backported by hand:

1. **Downgrade the SDK version** in `eboot.bin` and every `sce_module` PRX.
   It is a PAIR — PS4 at `param+0x10`, PS5 at `param+0x14` — and the kernel
   reports both: `SDK vesion: PS4:09040001 PPR:04000031`. Writing one half
   lets a title pass the launch gate and then die.
   Every shipped backport targets the FW 4 pair `(0x04000031, 0x09040001)`,
   because a 4.xx backport runs on every firmware above it.

2. **Supply the newer system libraries** in a `fakelib/` folder beside the
   game. Without them the game reaches its first missing API and aborts with
   `=== Call to unpatched function is detected!!! ===`.

3. **Union-mount that folder** over the sandbox's `common/lib` at launch.
   BackPork does this today, as a separate payload the user must run.

4. **Optionally patch `sce_module/libc.prx`** — one same-length symbol swap.
   Documented as helping *some* titles. It is not free: applying it to a title
   that did not need it crashed the game after 5 modules where it otherwise
   loaded 70 and ran.

Steps 1 and 4 shipped in the previous commit. This design covers 2 and 3, and
puts all four behind one button.

## Non-goals

- **We do not ship fakelib libraries.** They are Sony system binaries. Every
  comparable project refuses to distribute them, in those words. ps5upload
  publishes signed releases on every tag; bundling them risks the repository.
  Libraries come from the user's own console or a folder they supply.
- **We do not generate a backport from nothing.** Deciding which libraries a
  title needs is not reliably derivable — a scan of the dynamic imports
  over-predicted on 11 of 13 titles and still missed real dependencies.
- **We do not re-sign executables.** Patching in place is what shipped
  backports contain and what we verified running.

## Architecture

Four units, each usable and testable on its own.

### 1. `fakelib_overlay` — payload

Replaces the external BackPork payload. Watches `SceSysCore` for a game exec
(`kqueue`, `EVFILT_PROC`, `NOTE_FORK|NOTE_EXEC|NOTE_TRACK`), resolves the
title id, and if `<sandbox>/app0/fakelib` exists, union-mounts it over
`<sandbox>/<run-dir>/common/lib`. Unmounts when the game exits.

Depends on: the sandbox unmount rule already added for stale-mount cleanup.

Must refuse to act when an external BackPork is already running — two
overlays on one target produce `unionfs_domount: The same unionfs mount is
prohibited`, and the game then starts without its libraries.

Reports state (`watching`, `mounted <title>`, `idle`) so the UI can say
whether the overlay is live rather than asking the user to know.

### 2. `donor_index` — client, no new backend

For every installed title, list `<source>/fakelib` and record name + size.
Two libraries with the same name and size are the same build; titles sharing
builds form a firmware family. Verified: within a family the files are
byte-identical (`libSceAmpr 55cd4c589233` is the same in three titles).

`appsInstalled` and `fsListDir` already return everything needed. Copying is
`fs/copy`, console-side, with no PC round trip.

### 3. `backport` — client state module

- `status(title)` — does its SDK exceed the console firmware; which donors
  fit; what is already installed; is the overlay live.
- `apply(title, opts)` — SDK downgrade, then copy the donor's libraries,
  **never removing one the title already has**. Within a family the shared
  ones are identical, so this is a union rather than a merge conflict; the
  rule exists for the rare unique library (one title ships a `libkernel.sprx`
  no other has).
- `undo(title)` — restore `.bak` files, remove only the libraries we copied.
  Requires recording what was copied.

### 4. UI — one panel in Games → Ready to play

A title whose SDK exceeds the console firmware shows **Backport**. The panel
states what will change, which game the libraries come from, and offers
Launch and Undo afterwards. The `libc.prx` option is a checkbox, off, with a
one-line warning that it helps some titles and stops others.

The standalone Fakelib and SDK Changer screens are removed; this replaces both.

## Data flow

    status  →  sdk_scan (SDK pair, source path)
            +  fsListDir <source>/fakelib      (what it has)
            +  donor_index                      (what it could have)
            +  overlay status                   (will it be mounted)

    apply   →  sdk_patch(title, 0x04000031, patch_libc=false)
            →  fs/copy donor lib → <source>/fakelib/   (per missing library)
            →  record what was copied
            →  report

## Error handling

- No suitable donor: say so plainly, name the family the title needs, and do
  not half-apply. An SDK downgrade with no libraries produces a title that
  launches and aborts — worse than not starting, because it looks like our bug.
- Copy fails midway: report which libraries landed; `undo` removes them.
- Overlay not running: the panel says so before the user launches, since a
  correct backport with no overlay looks identical to a broken one.
- Title not patchable (encrypted retail SELF): refuse with the reason.

## Testing

- `fakelib_overlay`: host-side self-tests for the sandbox path rule (already
  written), the already-mounted refusal, and title-id extraction.
- `donor_index`: pure function over a directory listing — families, donor
  ranking, missing-library computation. Table-driven against the real shape of
  the 13-title library.
- `backport`: mocked transport; asserts an SDK-only apply never happens, that
  the title's own libraries survive, and that undo removes exactly what apply
  added.
- Hardware: re-run the verified PPSA25411 recipe end to end through the UI and
  confirm 13.7 GB / 137 threads.

## Verified reference case

PPSA25411 (`backport=0` raw rip, requires FW 11.00) on FW 9.60:
eboot + both PRXs on the FW 4 pair, `libc.prx` *not* symbol-patched, its own
`libSceAmpr` + `libScePlayGo` kept, six libraries added from PPSA32785.
Result: runs, 13.7 GB resident, 137 threads.
