import { useCallback, useRef, useState } from "react";
import { FolderInput, RadioTower, Loader2 } from "lucide-react";
import { Button, Callout } from "../../components";
import { useTr } from "../../state/lang";
import { useRosterStore } from "../../state/roster";
import { appsInstalled } from "../../api/ps5";
import { hostOf, transferAddr } from "../../lib/addr";
import {
  importFakelibSet,
  pollFakelibScan,
  startFakelibScan,
  type ScanProgress,
  type ScanTitleInput,
} from "../../state/fakelibCorpus";

/** The two ways to get backport libraries.
 *
 *  We cannot ship Sony system libraries, so a new user always has an empty
 *  corpus and has to fill it from games they own. Rather than failing at the
 *  moment they try to back port something, this offers the two routes and
 *  explains what each needs. It is shown when the corpus is empty, and reachable
 *  from Settings and from the Backport panel at any time afterwards. */
export function LibrarySourcePicker({
  addr,
  consoleName,
  scanTitles,
  onChanged,
}: {
  addr: string;
  consoleName: string;
  /** Titles to walk when scanning. Empty when the console is unreachable, which
   *  disables that route rather than letting it fail on click. */
  scanTitles: ScanTitleInput[];
  onChanged: () => void;
}) {
  const tr = useTr();
  const fileInput = useRef<HTMLInputElement>(null);
  const [busy, setBusy] = useState<"import" | "scan" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [progress, setProgress] = useState<ScanProgress | null>(null);
  const [note, setNote] = useState<string | null>(null);
  /** Which console the sweep is currently reading, for the progress line. */
  const [scanningName, setScanningName] = useState<string | null>(null);
  const rosterProfiles = useRosterStore((s) => s.profiles);

  const runImport = useCallback(
    async (files: FileList) => {
      const list = Array.from(files);
      if (list.length === 0) return;
      setBusy("import");
      setError(null);
      setNote(null);
      try {
        // One import is one set, named after where it came from. Sets are
        // installed whole; mixing libraries between them makes a combination
        // no game has ever run.
        const source = list.length === 1 ? list[0].name : `${list.length} files`;
        const label = deriveLabel(list);
        const outcome = await importFakelibSet(label, source, list);
        if (outcome.duplicate) {
          setNote(tr("fakelibs_import_duplicate", undefined,
            "You already have these libraries — nothing was added."));
        } else {
          const ignored = outcome.ignored.length;
          setNote(ignored > 0
            ? tr("fakelibs_import_ok_ignored", { count: ignored },
                `Added. ${ignored} file(s) were not libraries and were skipped.`)
            : tr("fakelibs_import_ok", undefined, "Added."));
        }
        onChanged();
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(null);
        if (fileInput.current) fileInput.current.value = "";
      }
    },
    [onChanged, tr],
  );

  const runScan = useCallback(async () => {
    setBusy("scan");
    setError(null);
    setNote(null);
    setProgress(null);
    try {
      // Sweep EVERY console we know of, not just the active one. The corpus is
      // global and content-addressed, so a library harvested from one console
      // is usable when backporting on any other — collecting them in a single
      // pass is what makes a second console contribute instead of starting
      // from an empty corpus of its own.
      const seen = new Set<string>();
      const targets: { host: string; name: string }[] = [];
      const addTarget = (host: string, name: string) => {
        const key = hostOf(host);
        if (!key || seen.has(key)) return;
        seen.add(key);
        targets.push({ host, name: name || key });
      };
      addTarget(addr, consoleName);
      for (const p of rosterProfiles) addTarget(p.host, p.name);

      const totals = {
        added: [] as [string, number][],
        skipped: 0,
        withoutLibraries: 0,
        notBackported: 0,
        errors: [] as string[],
      };
      let reached = 0;

      for (const target of targets) {
        setScanningName(target.name);
        setProgress(null);
        let titles: ScanTitleInput[];
        try {
          // Ask each console for its own titles: the caller's `scanTitles` only
          // describes the active one.
          const apps = await appsInstalled(transferAddr(target.host));
          titles = apps.titles
            .filter((t) => !!t.source && !t.system)
            .map((t) => ({
              title_id: t.titleId,
              title_name: t.titleName,
              image_backed: t.imageBacked,
              source: t.source,
            }));
        } catch {
          // One console being asleep must not abort the sweep — the others
          // still contribute. Name it so the user knows what was skipped.
          totals.errors.push(tr("fakelibs_scan_unreachable", { name: target.name },
            `${target.name}: not reachable, skipped.`));
          continue;
        }
        reached += 1;
        if (titles.length === 0) continue;

        // Pass the HOST as the stable key: the display name comes from the
        // roster and changes when the user renames a console, which used to
        // make one machine count as two sightings.
        const id = await startFakelibScan(
          target.host, target.name, titles, hostOf(target.host),
        );
        // Poll rather than block: reading every library off a console takes
        // on the order of a minute, and a silent wait that long reads as a hang.
        for (;;) {
          await new Promise((r) => setTimeout(r, 700));
          const snapshot = await pollFakelibScan(id);
          if (!snapshot) break;
          setProgress(snapshot);
          if (snapshot.done) {
            totals.added.push(...snapshot.added);
            totals.skipped += snapshot.skipped;
            totals.withoutLibraries += snapshot.withoutLibraries;
            totals.errors.push(...snapshot.errors);
            break;
          }
        }
      }

      setNote(summariseSweep(totals, reached, tr));
      // Per-console failures are informational, not a failed sweep: show them
      // without throwing away the sets the reachable consoles did contribute.
      if (totals.errors.length > 0) setError(totals.errors.slice(0, 4).join("\n"));
      onChanged();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
      setScanningName(null);
    }
  }, [addr, consoleName, onChanged, rosterProfiles, tr]);

  // The sweep always has at least the active console to try, so the button
  // stays live even when this one is asleep: it then reports which console
  // could not be reached, which is far more useful than a dead control.
  const scanDisabled = busy !== null;
  const consoleCount = new Set(
    [addr, ...rosterProfiles.map((p) => p.host)].map(hostOf).filter(Boolean),
  ).size;

  return (
    <div className="space-y-3">
      {error ? (
        <Callout tone="error" title={tr("fakelibs_action_failed", undefined, "That didn't work")}>
          {error}
        </Callout>
      ) : null}
      {note ? <Callout tone="success" title={note} /> : null}

      <div className="grid gap-3 sm:grid-cols-2">
        <SourceCard
          icon={<FolderInput size={18} />}
          title={tr("fakelibs_import_title", undefined, "Import libraries")}
          body={tr("fakelibs_import_body", undefined,
            "Choose the .sprx files from a backport pack you already have. They are kept together as one set.")}
          action={
            <>
              <input
                ref={fileInput}
                type="file"
                multiple
                accept=".sprx,.prx"
                className="hidden"
                onChange={(e) => { if (e.target.files) void runImport(e.target.files); }}
              />
              <Button
                variant="primary"
                disabled={busy !== null}
                loading={busy === "import"}
                onClick={() => fileInput.current?.click()}
              >
                {tr("fakelibs_import_action", undefined, "Choose files…")}
              </Button>
            </>
          }
        />

        <SourceCard
          icon={<RadioTower size={18} />}
          title={tr("fakelibs_scan_title", undefined, "Scan consoles")}
          body={tr("fakelibs_scan_body", { consoles: consoleCount, count: scanTitles.length },
            `Collect libraries from games already backported on every console you have set up — ${consoleCount} console(s), ${scanTitles.length} titles on this one. They all feed one shared corpus.`)}
          action={
            <Button variant="secondary" disabled={scanDisabled} loading={busy === "scan"} onClick={() => void runScan()}>
              {tr("fakelibs_scan_action", undefined, "Scan")}
            </Button>
          }
        />
      </div>

      {scanningName || (progress && !progress.done) ? (
        <div className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
          <Loader2 size={14} className="animate-spin" />
          {/* Name the console as well as the title: during a sweep the counter
              restarts per console, and without the name that reads as the
              progress bar jumping backwards. */}
          <span>
            {scanningName
              ? tr("fakelibs_scan_console", { name: scanningName }, `Scanning ${scanningName}…`)
              : null}
            {progress && !progress.done
              ? ` ${tr("fakelibs_scan_progress", { done: progress.titlesDone, total: progress.titlesTotal, current: progress.current },
                  `Reading ${progress.titlesDone} of ${progress.titlesTotal}… ${progress.current}`)}`
              : null}
          </span>
        </div>
      ) : null}

      {progress?.added.length ? (
        <ul className="max-h-32 overflow-auto rounded bg-[var(--color-surface-3)] p-2 font-mono text-xs">
          {progress.added.map(([label, count]) => (
            <li key={label}>{label} — {count}</li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

function SourceCard({
  icon, title, body, action,
}: {
  icon: React.ReactNode; title: string; body: string; action: React.ReactNode;
}) {
  return (
    <div className="flex flex-col gap-2 rounded border border-[var(--color-border)] p-3">
      <div className="flex items-center gap-2 font-medium">{icon}{title}</div>
      <p className="flex-1 text-xs text-[var(--color-muted)]">{body}</p>
      <div>{action}</div>
    </div>
  );
}

/** A name the user will recognise later, from what they picked. */
export function deriveLabel(files: File[]): string {
  if (files.length === 0) return "Imported set";
  // Directory uploads carry a relative path; its first segment is the folder
  // the user actually chose, which is a far better name than a file name.
  const withPath = files.find((f) => (f as File & { webkitRelativePath?: string }).webkitRelativePath);
  const rel = (withPath as (File & { webkitRelativePath?: string }) | undefined)?.webkitRelativePath;
  if (rel && rel.includes("/")) return rel.split("/")[0];
  return files.length === 1 ? files[0].name.replace(/\.(sprx|prx)$/i, "") : `Imported set (${files.length} files)`;
}

export function summarise(p: ScanProgress, tr: (k: string, v?: Record<string, unknown>, f?: string) => string): string {
  if (p.added.length === 0) {
    return p.skipped > 0
      ? tr("fakelibs_scan_none_new", undefined, "Nothing new — you already have every set these consoles offer.")
      : tr("fakelibs_scan_none", undefined,
          "No backported games found, so there were no libraries to collect.");
  }
  return tr("fakelibs_scan_added", { count: p.added.length },
    `Added ${p.added.length} set(s).`);
}

/** Outcome of a sweep across several consoles, collapsed into one line.
 *
 *  `reached` is how many consoles actually answered: "nothing found" and
 *  "nothing could be asked" are different results, and reporting the second as
 *  the first is the bug that made a broken scan look like an empty console. */
export function summariseSweep(
  totals: {
    added: [string, number][];
    skipped: number;
    withoutLibraries: number;
    notBackported: number;
    errors: string[];
  },
  reached: number,
  tr: (k: string, v?: Record<string, unknown>, f?: string) => string,
): string {
  if (totals.added.length > 0) {
    return tr("fakelibs_scan_added", { count: totals.added.length },
      `Added ${totals.added.length} set(s).`);
  }
  if (reached === 0) {
    return tr("fakelibs_scan_unreachable_all", undefined,
      "No console could be reached. Wake one and try again.");
  }
  if (totals.skipped > 0) {
    return tr("fakelibs_scan_none_new", undefined,
      "Nothing new — you already have every set these consoles offer.");
  }
  // Worth its own line: "no backported games found" on a console that plainly
  // HAS fakelib folders reads as a broken scan, when in fact those titles were
  // never downgraded and what is in them is not a backport.
  if (totals.notBackported > 0) {
    return tr("fakelibs_scan_not_backported", { count: totals.notBackported },
      `${totals.notBackported} game(s) have a fakelib/ but were never downgraded, so their files are not a backport and were skipped.`);
  }
  return tr("fakelibs_scan_none", undefined,
    "No backported games found, so there were no libraries to collect.");
}
