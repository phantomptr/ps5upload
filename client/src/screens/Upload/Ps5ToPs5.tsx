import { useMemo, useState, type ReactNode } from "react";
import { ArrowRightLeft } from "lucide-react";

import { Button, Select } from "../../components";
import { Input } from "../../components/Input";
import { useTr } from "../../state/lang";
import { useRosterStore } from "../../state/roster";
import { useTransferStore, phaseForHost } from "../../state/transfer";
import { ps5SourceProblem, sourceCandidates } from "../../lib/ps5Source";

/** "From another PS5": copy a file or folder from another console in the roster to the one
 *  being viewed, through this engine. The job is the ordinary transfer job, so progress, speed
 *  and the finished card are the Upload screen's own: the screen passes its status card in
 *  `status`. */
export function Ps5ToPs5Card({
  host,
  status,
}: {
  host: string;
  status: ReactNode;
}) {
  const tr = useTr();
  const profiles = useRosterStore((s) => s.profiles);
  const candidates = useMemo(() => sourceCandidates(profiles, host), [profiles, host]);
  const [from, setFrom] = useState("");
  const [srcPath, setSrcPath] = useState("/data/");
  const [destPath, setDestPath] = useState("/data/");
  const phase = useTransferStore((s) => phaseForHost(s, host));
  const start = useTransferStore((s) => s.start);

  const fromHost = from || candidates[0]?.host || "";
  const problem = ps5SourceProblem({
    fromHost,
    srcPath,
    toHost: host,
    destPath,
  });
  const busy = phase.kind === "starting" || phase.kind === "running";

  if (candidates.length === 0) {
    return (
      <section className="mb-6 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4 text-sm">
        <h3 className="mb-1 flex items-center gap-2 font-medium">
          <ArrowRightLeft size={16} aria-hidden />
          {tr("ps5src_title", undefined, "From another PS5")}
        </h3>
        <p className="text-xs text-[var(--color-muted)]">
          {tr(
            "ps5src_need_two",
            undefined,
            "Add a second console to the roster to copy between consoles.",
          )}
        </p>
      </section>
    );
  }

  return (
    <section className="mb-6 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface-2)] p-4 text-sm">
      <h3 className="mb-1 flex items-center gap-2 font-medium">
        <ArrowRightLeft size={16} aria-hidden />
        {tr("ps5src_title", undefined, "From another PS5")}
      </h3>
      <p className="mb-3 text-xs text-[var(--color-muted)]">
        {tr(
          "ps5src_hint_v2",
          undefined,
          "Copies a file or folder from another console to this one: straight between the consoles when they can reach each other, otherwise through this computer (nothing is stored on it). Easier: select the files in Files and choose Send to another console.",
        )}
      </p>
      <div className="grid gap-3 sm:grid-cols-3">
        <Select
          label={tr("ps5src_from_console", undefined, "Source console")}
          value={fromHost}
          onChange={(e) => setFrom(e.target.value)}
          options={candidates.map((p) => ({ value: p.host, label: p.name }))}
        />
        <Input
          label={tr("ps5src_from_path", undefined, "Path on the source console")}
          value={srcPath}
          onChange={(e) => setSrcPath(e.target.value)}
          spellCheck={false}
        />
        <Input
          label={tr("ps5src_dest_path", undefined, "Destination path on this console")}
          value={destPath}
          onChange={(e) => setDestPath(e.target.value)}
          spellCheck={false}
        />
      </div>
      <div className="mt-3 flex items-center gap-3">
        <Button
          variant="primary"
          disabled={problem !== null || busy}
          onClick={() =>
            void start({
              sourceKind: "file",
              srcPath: srcPath.trim(),
              dest: destPath.trim(),
              addr: host,
              ps5Source: { fromAddr: fromHost },
            })
          }
        >
          {tr("ps5src_start", undefined, "Copy to this PS5")}
        </Button>
        {problem !== null && (
          <span className="text-xs text-[var(--color-muted)]">
            {problem === "no_source_path"
              ? tr(
                  "ps5src_need_source_path",
                  undefined,
                  "Enter the source path, starting with /",
                )
              : problem === "no_destination_path"
                ? tr(
                    "ps5src_need_dest_path",
                    undefined,
                    "Enter the destination path, starting with /",
                  )
                : null}
          </span>
        )}
      </div>
      {phase.kind !== "idle" && <div className="mt-3">{status}</div>}
    </section>
  );
}
