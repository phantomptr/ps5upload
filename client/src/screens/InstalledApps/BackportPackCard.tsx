import { useCallback, useMemo, useState } from "react";
import { AlertTriangle, FolderOpen, PackageOpen, RotateCcw } from "lucide-react";
import { Button, Callout, Spinner } from "../../components";
import {
  fsCopy,
  fsDelete,
  fsListDir,
  fsMkdir,
  startTransferFile,
  waitForJob,
  type InstalledTitle,
} from "../../api/ps5";
import { transferAddr } from "../../lib/addr";
import { isTauriEnv } from "../../lib/tauriEnv";
import { useTr } from "../../state/lang";
import { inspectBackportPack, importBackportPack } from "../../state/fakelibCorpus";
import {
  applyPackInstall,
  planPackInstall,
  undoPackInstall,
  PackInstallError,
  type BackportPack,
  type PackInstallPlan,
  type PackInstallRecord,
  type PackTransport,
} from "../../lib/backportPack";

const gb = (bytes: number) =>
  bytes >= 1 << 30
    ? `${(bytes / (1 << 30)).toFixed(2)} GB`
    : `${(bytes / (1 << 20)).toFixed(1)} MB`;

/** Install a community backport pack into an installed title.
 *
 *  A pack is the format backports actually circulate in, and it carries more
 *  than a library set: a pre-patched `eboot.bin` and replacement
 *  `sce_module/` modules as well as `fakelib/`. Until this existed the app
 *  could only ever apply the `fakelib/` third of one, which is why a title
 *  whose pack fixed it in the eboot could not be fixed here at all.
 */
export function BackportPackCard({
  host,
  title,
  disabled,
  onCorpusChanged,
}: {
  host: string;
  title: InstalledTitle;
  /** The overlay is unavailable, so installing would produce a title that
   *  cannot mount its libraries. */
  disabled?: boolean;
  onCorpusChanged: () => void;
}) {
  const tr = useTr();
  const [path, setPath] = useState("");
  const [pack, setPack] = useState<BackportPack | null>(null);
  const [plan, setPlan] = useState<PackInstallPlan | null>(null);
  const [record, setRecord] = useState<PackInstallRecord | null>(null);
  const [busy, setBusy] = useState<null | "inspect" | "install" | "undo">(null);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  // Memoised on `host`: rebuilding this every render would change the identity
  // of every callback below it on every render too.
  const transport: PackTransport = useMemo<PackTransport>(
    () => ({
      mkdirConsole: (p) => fsMkdir(transferAddr(host), p),
      // Overwrite: restoring a stashed original writes over what we installed.
      copyConsole: (from, to) => fsCopy(transferAddr(host), from, to, 0, true),
      uploadHost: async (from, to) => {
        const jobId = await startTransferFile(from, to, transferAddr(host));
        await waitForJob(jobId);
      },
      remove: (p) => fsDelete(transferAddr(host), p),
    }),
    [host],
  );

  const browse = useCallback(async () => {
    // Tauri-only: a browser file picker cannot hand back a real filesystem
    // path, and the engine needs one. The web build types it instead.
    const { open } = await import("@tauri-apps/plugin-dialog");
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked === "string") setPath(picked);
  }, []);

  const inspect = useCallback(async () => {
    if (!path.trim()) return;
    setBusy("inspect");
    setError(null);
    setNote(null);
    setPack(null);
    setPlan(null);
    try {
      const found = await inspectBackportPack(path.trim());
      setPack(found);
      if (!found.isPack) {
        setError(tr("pack_not_a_pack", undefined,
          "No fakelib/ in that folder, so it is not a backport pack."));
        return;
      }
      // Read what is there now so the plan only backs up files that exist.
      const dest = title.source.replace(/\/+$/, "");
      const listing = async (dir: string) => {
        try {
          return (await fsListDir(transferAddr(host), dir)).map((e) => e.name);
        } catch {
          return [];
        }
      };
      const [fakelib, sceModule, gamePrx, sceSys, root] = await Promise.all([
        listing(`${dest}/fakelib`),
        listing(`${dest}/sce_module`),
        listing(`${dest}/prx`),
        listing(`${dest}/sce_sys/about`),
        listing(dest),
      ]);
      setPlan(
        planPackInstall(found, title, path.trim(), {
          fakelib,
          sceModule,
          gamePrx,
          sceSys,
          eboot: root.includes("eboot.bin"),
        }),
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }, [host, path, title, tr]);

  const install = useCallback(async () => {
    if (!plan) return;
    setBusy("install");
    setError(null);
    setNote(null);
    try {
      const done = await applyPackInstall(plan, transport);
      setRecord(done);
      // The library third of the pack is reusable on any console, so it is
      // worth keeping even though the eboot is not. Duplicates are a no-op.
      try {
        await importBackportPack(plan.packRoot);
        onCorpusChanged();
      } catch {
        // Installing is what the user asked for; a corpus miss is not a
        // failed install and must not be reported as one.
      }
      setNote(tr("pack_installed", undefined,
        "Pack installed. The eboot came pre-patched, so no SDK patch was applied."));
    } catch (e) {
      if (e instanceof PackInstallError) setRecord(e.record);
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }, [plan, tr, onCorpusChanged, transport]);

  const undo = useCallback(async () => {
    if (!record) return;
    setBusy("undo");
    setError(null);
    try {
      await undoPackInstall(record, transport);
      setRecord(null);
      setNote(tr("pack_undone", undefined, "Put back what the pack replaced."));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }, [record, tr, transport]);

  return (
    <div className="rounded-lg border border-[var(--color-border)] p-3 flex flex-col gap-2">
      <div className="flex items-center gap-2 text-sm font-medium">
        <PackageOpen size={15} />
        {tr("pack_title", undefined, "Install a backport pack")}
      </div>
      <p className="text-xs text-[var(--color-fg-muted)]">
        {tr("pack_body", undefined,
          "A downloaded pack folder holding fakelib/, a pre-patched eboot.bin and sce_module/. Everything it replaces is backed up first, so Undo puts the game back.")}
      </p>

      <div className="flex gap-2">
        <input
          className="flex-1 rounded border border-[var(--color-border)] bg-transparent px-2 py-1 text-xs"
          placeholder={tr("pack_path_placeholder", undefined, "Folder holding the pack")}
          value={path}
          onChange={(e) => setPath(e.target.value)}
        />
        {isTauriEnv() ? (
          <Button variant="secondary" onClick={() => void browse()} leftIcon={<FolderOpen size={14} />}>
            {tr("pack_browse", undefined, "Choose…")}
          </Button>
        ) : null}
        <Button variant="secondary" onClick={() => void inspect()} disabled={!path.trim() || busy !== null}>
          {busy === "inspect" ? <Spinner size={14} /> : tr("pack_inspect", undefined, "Check")}
        </Button>
      </div>

      {pack && pack.isPack ? (
        <div className="text-xs text-[var(--color-fg-muted)] flex flex-col gap-0.5">
          <div>
            {tr("pack_found", {
              libs: pack.libraries.length,
              mods: pack.sceModules.length + pack.gamePrx.length + pack.sceSys.length,
              size: gb(pack.totalBytes),
            }, `${pack.libraries.length} libraries, ${pack.sceModules.length + pack.gamePrx.length + pack.sceSys.length} module/plugin file(s)${pack.eboot ? ", a pre-patched eboot" : ""} — ${gb(pack.totalBytes)}`)}
          </div>
          {pack.other.length > 0 ? (
            <div>
              {tr("pack_other_ignored", { count: pack.other.length },
                `${pack.other.length} other file(s) in the folder are not part of a backport and will be left alone.`)}
            </div>
          ) : null}
        </div>
      ) : null}

      {plan?.titleMismatch ? (
        <Callout tone="warn" title={tr("pack_mismatch_title", undefined, "This pack names a different game")}>
          {tr("pack_mismatch_body", { pack: pack?.titleIdHint ?? "", target: title.titleId },
            `The folder says ${pack?.titleIdHint}, but you are installing into ${title.titleId}. Installing replaces this game's eboot with that one's.`)}
        </Callout>
      ) : null}

      {error ? (
        <Callout tone="error" title={tr("pack_failed", undefined, "Could not install the pack")}>
          {error}
        </Callout>
      ) : null}
      {note ? <div className="text-xs text-[var(--color-ok)]">{note}</div> : null}

      <div className="flex gap-2">
        <Button
          variant="primary"
          onClick={() => void install()}
          disabled={!plan || busy !== null || !!disabled || !!record}
          loading={busy === "install"}
        >
          {tr("pack_install", undefined, "Install pack")}
        </Button>
        {record ? (
          <Button variant="secondary" onClick={() => void undo()} disabled={busy !== null}
            leftIcon={<RotateCcw size={14} />} loading={busy === "undo"}>
            {tr("pack_undo", undefined, "Undo")}
          </Button>
        ) : null}
      </div>

      {disabled ? (
        <div className="flex gap-2 text-xs text-[var(--color-warn)]">
          <AlertTriangle size={14} />
          {tr("pack_overlay_needed", undefined,
            "The library overlay is unavailable, so the game could not mount these libraries.")}
        </div>
      ) : null}
    </div>
  );
}
