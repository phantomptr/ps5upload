// Convert a game folder or mount image (.exfat / .ffpkg) into an installable
// debug FPKG, then hand the result straight to the stream installer.
//
// Everything happens in the engine — desktop, Docker or Android; the console is
// only involved at the install step. The engine's work runs as a job, so this
// screen starts one and polls it like the transfer screens do.

import { useCallback, useEffect, useRef, useState } from "react";

import { PackagePlus } from "lucide-react";

import { Button, Callout, Card, PageHeader, ProgressBar } from "../../components";
import { fpkg, type FpkgInspection } from "../../api/fpkg";
import { jobCancel, jobStatus, type JobSnapshot } from "../../api/ps5";
import { pickPath } from "../../lib/pickPath";
import { isAndroid, isIOS } from "../../lib/platform";
import { useConnectionStore } from "../../state/connection";
import { usePkgLibrary } from "../../state/pkgLibrary";
import { useTr } from "../../state/lang";

const POLL_MS = 500;

function prettyBytes(n: number): string {
  if (n >= 1 << 30) return `${(n / (1 << 30)).toFixed(2)} GiB`;
  if (n >= 1 << 20) return `${(n / (1 << 20)).toFixed(1)} MiB`;
  return `${n} B`;
}

export default function FpkgConvertScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const installStream = usePkgLibrary(host, (s) => s.installStream);

  const [source, setSource] = useState("");
  const [outputDir, setOutputDir] = useState("");
  const [inspection, setInspection] = useState<FpkgInspection | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [jobId, setJobId] = useState<string | null>(null);
  const [job, setJob] = useState<JobSnapshot | null>(null);
  const [installing, setInstalling] = useState(false);
  const [installResult, setInstallResult] = useState<string | null>(null);
  const pollRef = useRef<number | null>(null);

  const check = useCallback(
    async (path: string, out: string) => {
      if (!path.trim()) return;
      setBusy(true);
      setError(null);
      setInspection(null);
      try {
        setInspection(await fpkg.inspect(path.trim(), out.trim() || undefined));
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(false);
      }
    },
    [],
  );

  const browse = useCallback(
    async (mode: "file" | "folder") => {
      try {
        const picked = await pickPath({
          mode,
          title:
            mode === "folder"
              ? tr("fpkg.pickFolder", undefined, "Choose the game folder")
              : tr("fpkg.pickImage", undefined, "Choose an .exfat or .ffpkg image"),
          filters:
            mode === "file"
              ? [{ name: "Game image", extensions: ["exfat", "ffpkg"] }]
              : undefined,
        });
        if (picked) {
          setSource(picked);
          void check(picked, outputDir);
        }
      } catch {
        // No native dialog (the browser build reads the engine's own disk):
        // the text field and the server-side picker are the way in there.
        setError(
          tr(
            "fpkg.noPicker",
            undefined,
            "This build cannot browse this machine — type a path the engine can see.",
          ),
        );
      }
    },
    [check, outputDir, tr],
  );

  // Poll the job until it stops running.
  useEffect(() => {
    if (!jobId) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const snapshot = await jobStatus(jobId);
        if (cancelled) return;
        setJob(snapshot);
        if (snapshot.status === "running") {
          pollRef.current = window.setTimeout(tick, POLL_MS);
        } else {
          setJobId(null);
        }
      } catch (e) {
        if (!cancelled) {
          setError(e instanceof Error ? e.message : String(e));
          setJobId(null);
        }
      }
    };
    pollRef.current = window.setTimeout(tick, POLL_MS);
    return () => {
      cancelled = true;
      if (pollRef.current !== null) window.clearTimeout(pollRef.current);
    };
  }, [jobId]);

  const convert = useCallback(async () => {
    if (!source.trim()) return;
    setError(null);
    setInstallResult(null);
    setJob(null);
    try {
      const { job_id } = await fpkg.build({
        source: source.trim(),
        outputDir: outputDir.trim() || undefined,
      });
      setJobId(job_id);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [source, outputDir]);

  const install = useCallback(async () => {
    if (!job?.dest) return;
    setInstalling(true);
    setError(null);
    try {
      const r = await installStream(job.dest, host);
      setInstallResult(
        r.ok
          ? tr("fpkg.installStarted", undefined, "Install handed to the console.")
          : (r.message ?? tr("fpkg.installFailed", undefined, "Install failed.")),
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setInstalling(false);
    }
  }, [job, host, installStream, tr]);

  const running = job?.status === "running" || jobId !== null;
  const warnings = inspection?.checks.filter((c) => !c.ok) ?? [];
  const passed = inspection?.checks.filter((c) => c.ok) ?? [];
  // Desktop and Android can open a real-path picker; the browser build reads
  // the engine's disk instead, where a typed path (or the server picker) is
  // the only route.
  const canBrowse = !isIOS() && (isAndroid() || "__TAURI_INTERNALS__" in window);

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-4 p-4">
      <PageHeader
        icon={PackagePlus}
        title={tr("fpkg.title", undefined, "Convert to FPKG")}
        description={tr(
          "fpkg.subtitle",
          undefined,
          "Turn a game folder or mount image into an installable package. The work runs on this machine, not the console.",
        )}
      />

      <Card>
        <div className="flex flex-col gap-2">
          <label className="text-sm font-medium" htmlFor="fpkg-source">
            {tr("fpkg.source", undefined, "Game source")}
          </label>
          <input
            id="fpkg-source"
            className="w-full rounded border border-neutral-700 bg-neutral-900 px-3 py-2 text-sm"
            placeholder={tr(
              "fpkg.sourcePlaceholder",
              undefined,
              "/games/PPSA09519.exfat, /games/my-game, /games/game.ffpkg",
            )}
            value={source}
            onChange={(e) => setSource(e.target.value)}
            onBlur={() => void check(source, outputDir)}
          />
          <div className="flex flex-wrap gap-2">
            {canBrowse && (
              <>
                <Button onClick={() => void browse("folder")} disabled={busy}>
                  {tr("fpkg.browseFolder", undefined, "Browse folder…")}
                </Button>
                <Button onClick={() => void browse("file")} disabled={busy}>
                  {tr("fpkg.browseImage", undefined, "Browse image…")}
                </Button>
              </>
            )}
            <Button onClick={() => void check(source, outputDir)} disabled={busy || !source.trim()}>
              {tr("fpkg.check", undefined, "Check")}
            </Button>
          </div>
          <label className="mt-2 text-sm font-medium" htmlFor="fpkg-output">
            {tr("fpkg.output", undefined, "Output folder")}
          </label>
          <input
            id="fpkg-output"
            className="w-full rounded border border-neutral-700 bg-neutral-900 px-3 py-2 text-sm"
            placeholder={tr("fpkg.outputHint", undefined, "Default: ~/Downloads/fpkgs")}
            value={outputDir}
            onChange={(e) => setOutputDir(e.target.value)}
          />
        </div>
      </Card>

      {error && (
        <Callout tone="error" title={tr("fpkg.error", undefined, "Conversion error")}>
          {error}
        </Callout>
      )}

      {inspection && (
        <Card>
          <div className="flex flex-col gap-2 text-sm">
            <div className="font-medium">
              {inspection.title ?? inspection.content_id ?? inspection.source}
            </div>
            <div className="text-neutral-400">{inspection.source}</div>
            <div className="flex flex-wrap gap-x-6 gap-y-1 text-neutral-300">
              <span>
                {tr("fpkg.files", undefined, "Files")}: {inspection.files}
              </span>
              <span>
                {tr("fpkg.size", undefined, "Game size")}: {prettyBytes(inspection.bytes)}
              </span>
              <span>
                {tr("fpkg.planned", undefined, "Package")}: {prettyBytes(inspection.planned_size)}
              </span>
              {inspection.output_free != null && (
                <span>
                  {tr("fpkg.free", undefined, "Free")}: {prettyBytes(inspection.output_free)}
                </span>
              )}
            </div>
            {inspection.content_id && (
              <div className="text-neutral-400">
                {tr("fpkg.contentId", undefined, "Content id")}: {inspection.content_id}
              </div>
            )}
            {inspection.required_firmware && (
              <div className="text-neutral-400">
                {tr("fpkg.firmware", undefined, "Requires firmware")}:{" "}
                {inspection.required_firmware}
              </div>
            )}
            {warnings.length > 0 && (
              <div className="mt-2 flex flex-col gap-1">
                {warnings.map((c) => (
                  <div key={c.name} className="text-amber-400">
                    {c.name}: {c.detail}
                  </div>
                ))}
              </div>
            )}
            {warnings.length === 0 && (
              <div className="mt-2 text-emerald-400">
                {tr("fpkg.ready", undefined, "Everything the package needs is here.")}
              </div>
            )}
            <details className="mt-1">
              <summary className="cursor-pointer text-neutral-400">
                {passed.length} {tr("fpkg.checks", undefined, "checks passed")}
              </summary>
              <ul className="mt-1 flex flex-col gap-0.5 text-neutral-500">
                {passed.map((c) => (
                  <li key={c.name}>
                    {c.name} — {c.detail}
                  </li>
                ))}
              </ul>
            </details>
          </div>
        </Card>
      )}

      {running && (
        <Card>
          <div className="flex flex-col gap-2">
            <ProgressBar
              value={job && job.total_bytes ? (job.bytes_sent ?? 0) / job.total_bytes : 0}
              tone="accent"
              label={tr("fpkg.converting", undefined, "Converting…")}
            />
            <div className="text-sm text-neutral-400">
              {prettyBytes(job?.bytes_sent ?? 0)} / {prettyBytes(job?.total_bytes ?? 0)}
            </div>
            <div>
              <Button
                variant="danger"
                onClick={() => {
                  if (jobId) void jobCancel(jobId);
                }}
              >
                {tr("fpkg.cancel", undefined, "Cancel")}
              </Button>
            </div>
          </div>
        </Card>
      )}

      {job?.status === "failed" && (
        <Callout tone="error" title={tr("fpkg.failed", undefined, "The conversion failed")}>
          {job.error}
        </Callout>
      )}

      {job?.status === "done" && (
        <Card>
          <div className="flex flex-col gap-2 text-sm">
            <div className="text-emerald-400">
              {tr("fpkg.done", undefined, "Package written")}
            </div>
            <div className="break-all text-neutral-300">{job.dest}</div>
            <div className="flex gap-2">
              <Button onClick={() => void install()} disabled={installing || !job.dest}>
                {installing
                  ? tr("fpkg.installing", undefined, "Installing…")
                  : tr("fpkg.install", undefined, "Install on the console")}
              </Button>
            </div>
            {installResult && <div className="text-neutral-300">{installResult}</div>}
          </div>
        </Card>
      )}

      <div>
        <Button
          onClick={() => void convert()}
          disabled={running || !source.trim() || (inspection !== null && inspection.files === 0)}
        >
          {tr("fpkg.convert", undefined, "Convert to FPKG")}
        </Button>
      </div>
    </div>
  );
}
