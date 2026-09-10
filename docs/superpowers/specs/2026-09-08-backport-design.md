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
- Overlay not active: because the overlay lives in our own payload it is live
  whenever the helper is, so the real cases are (a) the helper is down, which
  the connection state already reports, and (b) an external BackPork holds the
  mount, which we must detect and surface rather than silently doing nothing.
  Both matter because a correct backport with no overlay looks identical to a
  broken one.
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

---

## Revision, 2026-09-09 — hardware overturned two decisions

Three things in the design above are now known to be wrong. Recorded here
rather than silently edited, because each was believed on reasonable-looking
evidence and the reasons matter.

### 1. Libraries come from a local corpus of PROFILES, not from a live donor game

`donor_index` (§2) built its index by listing every installed game's
`fakelib/` at backport time. That still works, but it makes the set of
available libraries a function of what the user happens to have installed, and
it cannot help a user whose only game is the one they are trying to backport.

`scripts/gather-fakelibs.py` now collects the corpus once into `fakelibs/`
(gitignored — these are Sony binaries):

    fakelibs/
      manifest.json          34 profiles, 52 distinct builds
      profiles/<TITLE_ID>/   exactly what that title ships, unmodified

Sources are folder- and image-backed titles alike; an image's `fakelib/` is
readable straight through its ShadowMount+ mount.

### 2. A "complete set" is actively harmful — install a whole profile

The first version of the corpus did what it seemed obvious to do: one build per
library name, newest SDK wins, unioned across every game. **Installing it stopped
a working game from launching.** Red Dead Redemption ran on the 3 libraries its
ripper shipped and produced no process at all on our 13. The 3 libraries the
sets shared were byte-identical, so the damage came entirely from the 10 added —
most likely `libkernel.sprx`, which "newest wins" picked at a FW 13 pair and
handed to a game whose other libraries are all patched to 4.00.

A synthesised union is a combination no game has ever shipped, so nothing has
ever tested it. The profile — one real game's set, kept whole — is the only
unit with evidence behind it.

Nor can the right profile be *derived*. Within one declared SDK major,
`libSceAgc` has four distinct builds; a library's own embedded SDK pair records
only whether its ripper patched it. Selection is therefore ranked-and-tried,
not computed: same SDK major first, then FEWEST libraries (extra libraries are
what caused the failure), with Undo and "try the next profile" as the real
workflow. `rankProfiles` orders candidates; it does not claim to answer.

### 3. Eligibility keys on `sdkVersion`, never `requiredSystemSoftwareVersion`

SILENT HILL 2 declares required FW 10.20, was built with SDK 9.00, and runs on
a 9.60 console with no `fakelib/` at all. Across 31 image-backed titles, every
title with `sdkVersion` above the console firmware ships a fakelib and every
title below it does not — no exceptions. `requiredSystemSoftwareVersion`
predicts nothing and would offer pointless backports.

### Image-backed titles are in scope after all

§4 assumed disk-image titles could not be patched. They can, and it needs no
payload change — `/mnt/shadowmnt` is already in the payload path allowlist.
ShadowMount+ supports `image_rw=<image filename>` in its config, and that config
is live-watched, so:

    1. add `image_rw=<basename>` inside a ps5upload marker block in config.ini
    2. rename the image WITHIN ITS OWN DIRECTORY, wait for
       "[IMG][LVD] Source removed, unmounting", rename back  (~45 s)
    3. patch and copy through the now read-write mount
    4. strip the block, blip again -> back to read-only

`engine/crates/ps5upload-core/src/smp_image_rw.rs` owns the config rewriting.
This replaces the `smp_checkout` rename-out-of-the-scan-root flow for editing:
the image never leaves its directory or crosses a volume, and it is renamed for
~20 s rather than the whole session. Verified on exfatfs and ufs.

ufs images (`.ffpkg`) need one extra step: they carry real POSIX modes and ship
0555, so writes fail with EACCES even though the mount really is read-write.
`fs/chmod` to 0777 first, then restore the mode. exfat fakes 0777 and needs
nothing. Every one of the 31 images had >=94 MB free, so space is not a
constraint.

### Still open

- Startup recovery now distinguishes an idle stale session (safe to revert)
  from a running image-backed game (warn only). The client exposes the
  on-console journal so an interrupted read-write session remains visible and
  finishable after an app restart.
- No image-backed title has yet been backported from a genuinely un-backported
  starting state — every rip on hand that needs a backport already has one.
- The "does it work" oracle is still weak for SUCCESS. Thread count misled
  three times (1 / 18 / 263 threads) and must not be used. "No process N
  seconds after launch" is sound for FAILURE; success still needs a human
  looking at the screen.

### Telling the two failure modes apart

Both failures look identical in the process table (no process), but klog
separates them, and they need opposite fixes:

| klog after launch | meaning | fix |
|---|---|---|
| `=== Call to unpatched function is detected!!! ===` | libraries MISSING | a profile with more libraries |
| `createApp`, then death, no unpatched-function line | libraries WRONG | a different profile, usually a smaller one |

Measured on Red Dead: stripped of all libraries it produced one
unpatched-function line; loaded with a 13-library synthetic set it produced
none. The Backport panel should read klog after a trial launch and say which
of the two happened rather than reporting a bare failure.


---

## Revision 2, 2026-09-09 — storage is content-addressed, and the earlier
## "one set breaks games" result is UNCONFIRMED

### The A/B/C/D trial failed its own control

Red Dead was stripped of libraries and rebuilt four ways. Result:

| step | libraries installed | outcome |
|---|---|---|
| A | none | no process, 1 unpatched-function line |
| B | Ghost of Yotei's set | no process, 0 unpatched lines |
| C | PRAGMATA's set (different builds) | no process, 0 unpatched lines |
| D | Red Dead's own set | ran, 9/9 samples, 40 threads |

**B and D installed byte-identical files** (sha256 match, 3 of 3). B failed and D
worked, so this harness cannot attribute a launch failure to library content.
That invalidates B and C as evidence about donor compatibility, and it equally
undermines the earlier single-trial result that a 13-library synthetic set broke
Red Dead — same rig, same shape of evidence. In both experiments the LAST cycle
is the one that worked, which points at an uncontrolled variable around
launching soon after a ShadowMount+ remount.

Nothing about library compatibility should be asserted until a repeated,
order-randomised trial with the control run more than once. What A does show,
and is consistent with everything else, is that libraries are required at all
and that missing ones announce themselves in klog.

### Storage: builds, not per-game directories

The corpus is small and heavily shared — across 34 titles there are **13 library
names and 52 distinct builds**, and the commonest `libSceAgc` build ships in 13
of them. One directory per source game stored 15 MiB of real content as 58 MiB
(3.8x) and buried the thing that actually varies: which BUILD you have.

    fakelibs/
      manifest.json                 <- libraries[] + observed_sets[]
      builds/<library>/<sha8>.sprx  <- every distinct build, stored once

`libraries[]` lists each name with its builds and, per build, `shipped_by` —
the evidence for choosing between them, since a build 13 games use is better
travelled than a singleton. `observed_sets[]` records which build each real game
ships, as references. A set is still the unit the UI offers, but it is now
metadata, so recording every one duplicates nothing and the corpus can express
sets no single game shipped if evidence ever supports that.

`resolveSets()` turns a manifest into installable sets and validates as it goes:
title id, library name, sha256, and `path` (which must stay inside `builds/`,
since it is concatenated into a path we copy from). A set referencing a build
the corpus lacks, or a build filed under a different name, is dropped whole
rather than half-installed.

`gather-fakelibs.py --from-existing` re-lays-out a corpus already on disk with
no console, and the emit step now builds into `fakelibs.new` and swaps: writing
in place destroyed the corpus once, because `--from-existing` reads its sources
from inside `fakelibs/` and the old code wiped the directory first.

---

## Revision 3 — which build do you use when a library has several?

Measured across all 34 titles. The answer has three layers, and the first two
dissolve most of the apparent choice.

### 1. Many "different builds" are the same library, differently stamped

Two `libSceAmpr` builds with different sha256 and identical size differ in
exactly 36 bytes: a 32-byte digest at `0x510`, and the 8-byte SDK pair in the
param segment. Mask those two regions and the files are byte-identical.

    82484486 @0x26f50: 01 00 05 08 09 00 00 02   ps4=08050001 ps5=02000009 (unpatched)
    d906bb7b @0x26f50: 01 00 04 09 31 00 00 04   ps4=09040001 ps5=04000031 (patched to FW4)

So they are one library at two patch states, and our own SDK patcher moves
either to any pair. The manifest now carries `code_id` per build — the hash
with those regions masked. **Equal `code_id` means the choice is a non-choice.**

Across the corpus the stamps split roughly evenly: 14 builds at the FW4 pair,
13 unpatched at `08050001/02000009`, one libkernel at `13090001/12000043`.

### 2. Genuinely different builds travel in HARVESTS, so you pick a harvest

The 13 titles shipping `libSceAgc e1c8f6dc` are *exactly* the 13 shipping
`libSceAgcDriver d83edb3a` and `libScePsml 6632020e`. Same for the 10-title and
3-title groups. These are one rip kit's libraries, lifted from one firmware.
Choosing a build per library independently is what manufactures combinations
nobody ever shipped. The manifest now derives `harvests[]` from co-occurrence:
builds shipped by exactly the same title set.

The graphics trio (Agc / AgcDriver / Psml) is harvest-locked. The service
libraries (Ampr, AppContent, GameUpdate, NpEntitlementAccess, PlayGo) vary
*within* a harvest — but see (1): much of that variation is stamp-only.

### 3. What does NOT select a build

- **The target game's SDK.** `e1c8f6dc` ships in titles with SDK 0500, 0900,
  1000 and 1100; `eace8ac1` spans 0400-1200. One SDK (1100) uses four different
  libSceAgc builds. `rankProfiles` used to sort on "same SDK major first"; that
  ordering was noise and has been REMOVED (the function no longer takes the
  target SDK at all).
- **The build's own SDK pair.** It records what a ripper patched the file to,
  not where it came from (see 1).
- **Newest / largest.** The rule that produced the 13-library set picked an
  FW12 libkernel for an FW11 game.

The only signal with evidence behind it is **popularity**: prefer the harvest
that the most titles ship, because a combination 13 games run is better
travelled than a singleton. That is a default, not a prediction.

`rankProfiles` now orders by, in turn: the target's own set (the one
combination known to work for that exact game), then `setAttestation` — the
count of titles shipping the set's RAREST build, since a set is only as
well-travelled as its least common member — then fewest libraries, then title
id. On the recovered corpus this puts Red Dead's 3-library set first, which is
the set that actually ran on hardware in step D of the trial.

### Parser fix that changed the data

`sdk_pair` walked the ELF program headers and gave up on files where that walk
fails, which reported 27 of 52 builds as "sdk unknown" and made every
SDK-based ranking operate on absent data. The param magic occurs exactly once
in those files and the 0x20 size field in front of it confirms the hit, so
`param_site` now falls back to scanning. All 28 builds in the recovered corpus
now read their pair.


---

## Revision 4, 2026-09-10 — the harness itself is the problem

A controlled run settles the open question from Revision 2. Red Dead Redemption,
completely unchanged between attempts — no remount, no library change, no config
change:

    plain launch #1   RUNS (2/9 samples alive, peak 39 thr)
    plain launch #2   RUNS (9/9 samples alive, peak 39 thr)
    plain launch #3   NO PROCESS (0/9)

**Launching fails on its own roughly one time in three.** So:

  * The Revision 2 result ("a 13-library synthetic set stopped Red Dead
    launching") is REFUTED, not merely unconfirmed. It was one trial.
  * Steps B and C of the A/B/C/D trial say nothing, which was already implied by
    B failing its own byte-identical control.
  * Nothing in this document should be read as evidence about which libraries a
    game needs. The corpus structure, the harvest finding and the stamp-variant
    finding all rest on file contents rather than on launches, and are unaffected.

What survives about launching: libraries are required at all (stripped of every
library the game emits `Call to unpatched function` and does not start), and
that klog line is a positive signal — an actual kernel message rather than an
absence — so it is trustworthy from a single attempt. Its absence is not.

### Consequences for the product

`verdictFrom` judges the END of the sampling window, not whether any sample saw
a process: a title that starts and dies still had threads, and reporting its
peak is the same error that made thread count useless three times.

`combineAttempts` repeats a failure `FAILURE_ATTEMPTS` (3) times before
believing it. Any attempt ending with the title up wins outright — a game
cannot run by accident — and a `missing-libraries` verdict is believed
immediately on the strength of its kernel message. Anything else must be
unanimous, or the UI says it could not tell rather than blaming the libraries.

### How to test libraries from here

Repeat every configuration at least three times, randomise the order, and re-run
the known-good control inside the same session. A failure counts only when every
attempt agrees.
