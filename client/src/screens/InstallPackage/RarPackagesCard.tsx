import { useRef, useState } from "react";

import { type RarPackage } from "../../api/links";
import { Button, Spinner } from "../../components";
import { formatBytes } from "../../lib/format";
import { pickPath } from "../../lib/pickPath";
import {
  installRarPackages,
  isRarFirstVolume,
  listRarPackages,
  type RarInstallResult,
  type RarPhase,
} from "../../state/rarPackages";
import { useTr } from "../../state/lang";
import { RarPasswordPrompt } from "../Upload/RarPasswordPrompt";
import type { RarPasswordProblem } from "../../lib/rarPassword";

/** Install the packages inside a RAR (R6, #370): they are unpacked to the console over AVA1
 *  (no extraction on this computer) and each is queued as its own install, base first. */
export function RarPackagesCard({ host }: { host: string }) {
  const tr = useTr();
  const [archive, setArchive] = useState<string | null>(null);
  const [packages, setPackages] = useState<RarPackage[] | null>(null);
  const [password, setPassword] = useState<string | null>(null);
  const [pwProblem, setPwProblem] = useState<RarPasswordProblem | null>(null);
  const [busy, setBusy] = useState(false);
  const [phase, setPhase] = useState<RarPhase | null>(null);
  const [result, setResult] = useState<RarInstallResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const running = useRef(false);

  async function inspect(path: string, pw: string | null) {
    setError(null);
    setResult(null);
    const r = await listRarPackages(path, pw);
    if (r.password) {
      setPwProblem(r.password);
      setPackages(null);
      return;
    }
    setPwProblem(null);
    if (r.error) {
      setPackages(null);
      setError(r.error);
      return;
    }
    setPackages(r.packages);
  }

  async function choose() {
    const picked = await pickPath({
      mode: "file",
      title: tr("rarpkg.pick", undefined, "Choose a RAR archive"),
      filters: [{ name: "RAR", extensions: ["rar"] }],
    });
    if (!picked) return;
    if (!isRarFirstVolume(picked)) {
      setError(
        tr(
          "rarpkg.notFirst",
          undefined,
          "Choose the first part of the archive (the .rar or .part1.rar file).",
        ),
      );
      return;
    }
    setArchive(picked);
    setPassword(null);
    await inspect(picked, null);
  }

  async function run() {
    if (!archive || running.current) return;
    running.current = true;
    setBusy(true);
    setResult(null);
    setError(null);
    try {
      const r = await installRarPackages({
        host,
        archivePath: archive,
        password,
        onPhase: setPhase,
      });
      if (r.password) setPwProblem(r.password);
      setResult(r);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      running.current = false;
      setBusy(false);
      setPhase(null);
    }
  }

  const phaseText = (() => {
    if (!phase) return null;
    switch (phase.phase) {
      case "listing":
        return tr("rarpkg.listing", undefined, "Reading the archive…");
      case "unpacking":
        return phase.total > 0
          ? tr(
              "rarpkg.unpacking",
              { sent: formatBytes(phase.sent), total: formatBytes(phase.total) },
              "Unpacking to the PS5: {sent} of {total}",
            )
          : tr("rarpkg.unpackingNoTotal", undefined, "Unpacking to the PS5…");
      case "reading":
        return tr(
          "rarpkg.reading",
          { n: phase.index, count: phase.count },
          "Checking package {n} of {count}…",
        );
      case "installing":
        return tr(
          "rarpkg.installing",
          { n: phase.index, count: phase.count, name: phase.name },
          "Installing {n} of {count}: {name}",
        );
    }
  })();

  return (
    <div className="mb-4 rounded-md border border-[var(--color-border)] bg-[var(--color-surface-2)] p-3">
      <div className="text-sm font-medium text-[var(--color-text)]">
        {tr("rarpkg.title", undefined, "Install packages from a RAR archive")}
      </div>
      <p className="my-1 text-xs text-[var(--color-muted)]">
        {tr(
          "rarpkg.help",
          undefined,
          "Packages inside the archive, in any folder, are unpacked to the PS5 over your network (nothing is extracted on this computer) and installed one by one, the base game before its update. If an install fails, the unpacked files stay on the PS5.",
        )}
      </p>
      <div className="flex flex-wrap items-center gap-2">
        <Button variant="secondary" size="sm" onClick={choose} disabled={busy}>
          {tr("rarpkg.choose", undefined, "Choose RAR…")}
        </Button>
        {archive && (
          <span className="min-w-0 truncate text-xs text-[var(--color-muted)]" title={archive}>
            {archive.replace(/\\/g, "/").split("/").pop()}
          </span>
        )}
      </div>
      {pwProblem && archive && (
        <RarPasswordPrompt
          problem={pwProblem}
          onSubmit={(pw) => {
            setPassword(pw);
            void inspect(archive, pw);
          }}
        />
      )}
      {packages && packages.length > 0 && (
        <div className="mt-2">
          <ul className="mb-2 max-h-32 overflow-auto text-xs text-[var(--color-muted)]">
            {packages.map((p) => (
              <li key={p.path} className="truncate">
                {p.path} ({formatBytes(p.size)})
              </li>
            ))}
          </ul>
          <Button variant="primary" size="sm" onClick={run} disabled={busy}>
            {tr(
              "rarpkg.run",
              { count: packages.length },
              "Unpack and install {count} package(s)",
            )}
          </Button>
        </div>
      )}
      {packages && packages.length === 0 && (
        <p className="mt-2 text-xs text-[var(--color-warn)]">
          {tr("rarpkg.none", undefined, "That archive has no .pkg files in it.")}
        </p>
      )}
      {busy && phaseText && (
        <div className="mt-2 flex items-center gap-2 text-xs">
          <Spinner size={14} tone="accent" />
          <span>{phaseText}</span>
        </div>
      )}
      {result && (
        <div className="mt-2 text-xs" role="status">
          <p className={result.ok ? "text-[var(--color-good)]" : "text-[var(--color-warn)]"}>
            {result.message}
          </p>
          <ul className="mt-1 space-y-0.5">
            {result.outcomes
              .filter((o) => o.status !== "installed")
              .map((o) => (
                <li key={o.entry} className="text-[var(--color-muted)]">
                  {o.entry}: {o.message}
                </li>
              ))}
          </ul>
        </div>
      )}
      {error && (
        <p role="alert" className="mt-2 text-xs text-[var(--color-bad)]">
          {error}
        </p>
      )}
    </div>
  );
}
