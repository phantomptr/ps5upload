# Acquiring backport libraries, from inside ps5upload

Companion to `2026-09-08-backport-design.md`, which covers what a backport is
and what the corpus contains. This one covers how a user GETS a corpus, and
what the Backport button does when they have none.

## The problem

Backporting needs replacement system libraries. They are Sony binaries, so we
cannot ship them: the user has to supply them from games they own. Until now
that meant running `scripts/gather-fakelibs.py` from a terminal, which is not a
feature — it is a prerequisite disguised as one.

Two things must become possible from the UI, at any time:

  * **Import** libraries the user already has (a pack from a forum, a folder
    they extracted, files a friend sent).
  * **Scan** the connected console and harvest the library sets from games that
    are already backported.

Both feed one persistent local corpus, reusable across every future backport.

## Decisions taken

| Question | Decision | Why |
|---|---|---|
| Where does the corpus live? | App-managed, hidden (`<app-data>/fakelibs/`) | No path to type, no path to get wrong. Desktop and browser behave identically because the engine owns the filesystem either way. |
| How are imported files grouped? | One import = one named set, kept whole | A set is only known to work as a unit. Pooling files by name manufactures combinations no game ever shipped -- the shape that stopped Red Dead launching. |
| What does Scan look at? | The connected console only | It is the walker already proven by `gather-fakelibs.py`, which produced the 34-set corpus. Local-folder and USB scanning are deliberately out of scope. |
| Where does the logic live? | The engine | It already has console file access; the browser build is otherwise a second-class citizen, which has bitten this codebase before. 58 MB of libraries never enters the renderer. |

## 1. Storage

One corpus, shared across consoles -- libraries are firmware-derived, not
console-specific.

    <app-data>/fakelibs/
      manifest.json
      builds/<library>/<sha8>.sprx

`manifest.json` becomes **schema 4**. `libraries[]` (each name, its builds,
`shipped_by`, `code_id`) and `harvests[]` (builds shipped by exactly the same
titles) are unchanged from schema 3. `observed_sets[]` is replaced by `sets[]`,
which differs in three ways:

  * a stable `id`, so a backport record and a Delete button can refer to a set
    that is no longer named after a game;
  * a user-facing `label`;
  * an `origin`, because a hand-imported pack and a harvested game are
    different kinds of evidence and the UI has to say which.

There is no migration: schema 3 was never released, and a corpus is rebuilt by
re-scanning. `resolveSets` reads schema 4 only and returns nothing for anything
else, which surfaces as "no libraries yet" rather than a crash.

    { "id": "set-7",
      "label": "FW11 pack (BackPork)",
      "origin": { "kind": "import", "source": "BackPork-FW11-pack.zip",
                  "at": "2026-09-09T15:04:00Z" },
      "libraries": { "libSceAgc.sprx": "<sha256>", ... } }

    { "id": "set-2",
      "label": "Red Dead Redemption",
      "origin": { "kind": "scan", "title_id": "PPSA30528",
                  "console": "PS5-Pro", "at": "..." },
      "libraries": { ... } }

Adding a set is content-addressed, so re-scanning is idempotent and re-importing
the same pack adds nothing. That is what makes "scan again any time" safe rather
than a way to accumulate duplicates.

Client-side, the type currently called `FakelibProfile` is renamed
`FakelibSet` and `rankProfiles` to `rankSets`, matching the vocabulary the UI
now uses. `BackportRecord.profileTitleId` becomes `setId` -- a persisted field,
so the storage key goes to `ps5upload.backports.v3`. A v2 record cannot name a
set that has an id, and honouring one would offer an undo that restores from the
wrong place.

## 2. Endpoints

| Endpoint | Does |
|---|---|
| `GET /api/fakelibs/corpus` | manifest + summary counts, for Settings and the panel |
| `POST /api/fakelibs/scan` | walk the console; returns sets found and which are new. A job -- it takes ~60 s over FTP |
| `POST /api/fakelibs/import` | multipart upload -> one new set |
| `DELETE /api/fakelibs/set/{id}` | remove a set; garbage-collect builds nothing else references |

The SELF param-locating needed for `code_id` and the SDK pair ports from
`gather-fakelibs.py` (~60 lines). Known-good test vectors exist: two
`libSceAmpr` builds and two `libScePlayGo` builds that differ only in a 32-byte
digest at `0x510` and the 8-byte SDK pair, and must collapse to one `code_id`.

## 3. The Backport flow

Entry state is derived from the corpus, never from a first-run flag, so the
flow self-heals if the corpus is later emptied.

    Backport > <title>
      corpus empty     -> chooser: [Import files...] [Scan console]
      corpus has sets  -> ranked set list + [Get more libraries...]

Ranking is `rankProfiles`: the target's own set first when the corpus has it,
then `setAttestation` (titles shipping the set's RAREST build), then fewest
libraries. It orders candidates; it does not claim to answer.

### Apply, verify, keep-or-undo

Nothing in the data predicts which set a game needs, so the design's job is to
make a try cheap and reversal certain -- not to guess well.

| Observation | What we may tell the user |
|---|---|
| no process after ~70 s, `Call to unpatched function` in klog | libraries MISSING -- offer a larger set |
| no process, `createApp` reached, no unpatched line | libraries WRONG -- offer the next set |
| process alive | **we may not claim success** -- ask them to confirm on screen |

The third row is deliberate. Thread count misled three times (1 / 18 / 263
threads), and the A/B/C/D trial failed its own byte-identical control. The panel
says "still running -- does it reach gameplay?" with Keep / Undo, and never
announces success on its own.

### As implemented

`verdictFrom(samples, klog, titleId)` judges on the END of the sampling window,
not on whether a process was ever seen. A title that starts and dies had
threads at some point, so "any sample alive" reports a peak thread count from a
run that is already over — the same mistake that made thread count useless
three times. Judging the last sample also tolerates a slow start: a cold start
from USB showed nothing for 40 seconds and reached 263 threads.

`nextSetsAfter(verdict, failed, ranked)` decides what to offer next, and the
failed set survives the undo so the proposal honours it. After a
missing-library failure only LARGER sets are offered, because a smaller one
cannot supply what was missing and trying it costs a full install-launch-undo
cycle. When nothing larger remains, that is its own message: no set you have
can help.

klog is drained before the launch. Otherwise a previous attempt's
unpatched-function line is read as this attempt's.

## 4. Managing the corpus (Settings)

    Settings > Backport libraries

      34 library sets, 52 builds, 15 MB
      Last updated: today, from PS5-Pro

      [ Import files... ]  [ Scan console ]  [ Reveal ]

Per-set rows show label, origin, library count, and Delete. No path field.

## 5. Failure modes this must survive

* **Empty corpus AND sleeping console** -- both acquisition paths are dead. Say
  so before the user clicks, rather than offering a button that fails.
* **Already-backported title** -- Red Dead has a `fakelib/` and still shows a
  Backport button today, because nothing checks. Detect it and lead with
  "already has N libraries -- Replace or Leave alone".
* **Junk import** -- `._*` AppleDouble sidecars, a whole game folder, a zip of
  screenshots. Accept only `.sprx`/`.prx`, reject dot-prefixed names, refuse an
  import with zero valid files, warn above ~20 (almost certainly a wrong
  folder).
* **Interrupted apply** -- `BackportApplyError` records `copiedPaths` and the
  record is written with `complete: false`; Undo stays available.
* **Interrupted image session** -- a stale `image_rw=` block left in SMP's
  config leaves a game image silently writable. `stale_session_action` decides
  the policy; the UI must surface it.
* **ufs images** (`.ffpkg`) carry real POSIX modes and ship 0555: `chmod` to
  0777 before writing, restore after. exfat fakes 0777 and needs nothing.
* **Free space** -- every measured image had >=94 MB free, but check before
  copying so a failure happens before anything is touched, not half-way.
* **Undo after the set was deleted** -- undo restores from the console-side
  stash (`/data/ps5upload/backport/<title>`), never from the corpus, so removing
  a set can never strand a game.
* **Deleting a set** garbage-collects only builds nothing else references;
  builds are shared up to 13 ways.
* **External BackPork holding the overlay** -- the kernel refuses a second
  unionfs (EDEADLK) and the game starts with no libraries. Existing `blocked`
  overlay state; kept.

## 6. Testing

* Unit (Rust): manifest round-trip, content-addressed add is idempotent,
  set deletion GCs only unreferenced builds, `code_id` collapses the four known
  stamp-variant vectors, import validation rejects the junk classes.
* Unit (client): entry-state selection from corpus contents, ranking order,
  already-backported detection, the three verify verdicts.
* Hardware: import a pack, scan the console, back up a title end-to-end on both
  an exfat and a ufs image, undo, and confirm the console is byte-identical to
  how it started.

## 7. Out of scope

Local-folder and USB scanning; sharing or publishing a corpus; editing a set's
contents after import; automatic selection that claims to predict success.
