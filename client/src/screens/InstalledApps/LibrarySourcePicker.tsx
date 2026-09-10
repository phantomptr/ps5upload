import { useCallback, useRef, useState } from "react";
import { FolderInput, RadioTower, Loader2 } from "lucide-react";
import { Button, Callout } from "../../components";
import { useTr } from "../../state/lang";
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
      const id = await startFakelibScan(addr, consoleName, scanTitles);
      // Poll rather than block: reading every library off the console takes
      // on the order of a minute, and a silent wait that long reads as a hang.
      for (;;) {
        await new Promise((r) => setTimeout(r, 700));
        const snapshot = await pollFakelibScan(id);
        if (!snapshot) break;
        setProgress(snapshot);
        if (snapshot.done) {
          setNote(summarise(snapshot, tr));
          break;
        }
      }
      onChanged();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }, [addr, consoleName, onChanged, scanTitles, tr]);

  const scanDisabled = busy !== null || scanTitles.length === 0;

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
          title={tr("fakelibs_scan_title", undefined, "Scan this console")}
          body={
            scanTitles.length === 0
              ? tr("fakelibs_scan_unavailable", undefined,
                  "Needs the console awake and connected. Nothing to scan right now.")
              : tr("fakelibs_scan_body", { count: scanTitles.length },
                  `Read the libraries from games on this console that are already backported. ${scanTitles.length} titles to check.`)
          }
          action={
            <Button variant="secondary" disabled={scanDisabled} loading={busy === "scan"} onClick={() => void runScan()}>
              {tr("fakelibs_scan_action", undefined, "Scan")}
            </Button>
          }
        />
      </div>

      {progress && !progress.done ? (
        <div className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
          <Loader2 size={14} className="animate-spin" />
          {tr("fakelibs_scan_progress", { done: progress.titlesDone, total: progress.titlesTotal, current: progress.current },
            `Reading ${progress.titlesDone} of ${progress.titlesTotal}… ${progress.current}`)}
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
      ? tr("fakelibs_scan_none_new", undefined, "Nothing new — you already have every set on this console.")
      : tr("fakelibs_scan_none", undefined,
          "No backported games found on this console, so there were no libraries to collect.");
  }
  return tr("fakelibs_scan_added", { count: p.added.length },
    `Added ${p.added.length} set(s).`);
}
