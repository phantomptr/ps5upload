// Swaps a run left unfinished on the console (lib/dumpSwap.ts journals every step there): each
// one gets the way out that fits where it stopped.

import { useCallback, useEffect, useState } from "react";

import { Button, Callout } from "../../components";
import {
  finishSwap,
  readJournals,
  rollbackSwap,
  type SwapDeps,
  type SwapJournal,
} from "../../lib/dumpSwap";
import { useTr } from "../../state/lang";

export function SwapJournalsView(props: {
  journals: SwapJournal[];
  busy: string | null;
  onRollback: (j: SwapJournal) => void;
  onFinish: (j: SwapJournal, choice: "delete" | "keep") => void;
}) {
  const tr = useTr();
  return (
    <>
      {props.journals.map((j) => (
        <Callout
          key={j.titleId}
          tone="warn"
          title={
            j.step === "unconfirmed"
              ? tr(
                  "fpkg.swapUnconfirmed",
                  { title: j.titleId, path: j.parked },
                  "The PS5 did not confirm the install of {title}. Check the game on the PS5 first. The dump is set aside at {path}.",
                )
              : tr(
                  "fpkg.swapPending",
                  { title: j.titleId, path: j.parked },
                  "An unfinished swap for {title}: the dump is set aside at {path}.",
                )
          }
        >
          <div className="flex flex-wrap gap-2">
            {j.step === "installed" ? (
              <>
                <Button size="sm" variant="danger" disabled={!!props.busy} onClick={() => props.onFinish(j, "delete")}>
                  {tr("fpkg.deleteDump", undefined, "Delete the old dump")}
                </Button>
                <Button size="sm" disabled={!!props.busy} onClick={() => props.onFinish(j, "keep")}>
                  {tr("fpkg.keepDump", undefined, "Keep it parked")}
                </Button>
              </>
            ) : j.step === "unconfirmed" ? (
              <>
                <Button size="sm" disabled={!!props.busy} onClick={() => props.onRollback(j)}>
                  {tr("fpkg.swapRollback", undefined, "Put the dump back")}
                </Button>
                <Button size="sm" disabled={!!props.busy} onClick={() => props.onFinish(j, "keep")}>
                  {tr("fpkg.keepDump", undefined, "Keep it parked")}
                </Button>
                <Button size="sm" variant="danger" disabled={!!props.busy} onClick={() => props.onFinish(j, "delete")}>
                  {tr("fpkg.deleteDump", undefined, "Delete the old dump")}
                </Button>
              </>
            ) : (
              <Button size="sm" disabled={!!props.busy} onClick={() => props.onRollback(j)}>
                {tr("fpkg.swapRollback", undefined, "Put the dump back")}
              </Button>
            )}
          </div>
        </Callout>
      ))}
    </>
  );
}

/** The journals to list: all but the swap the result card is showing (`shownTitle`). */
export function pendingJournals(all: SwapJournal[], shownTitle: string | null): SwapJournal[] {
  return shownTitle ? all.filter((j) => j.titleId !== shownTitle) : all;
}

/** Reads the console's swap journals once a console answers; hides while a run is going. */
export function SwapJournals(props: {
  deps: SwapDeps | null;
  hidden: boolean;
  /** The title whose swap the result card already offers to finish. */
  shownTitle: string | null;
}) {
  const tr = useTr();
  const [journals, setJournals] = useState<SwapJournal[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const { deps } = props;

  const load = useCallback(async () => {
    if (!deps) {
      setJournals([]);
      return;
    }
    setJournals(await readJournals(deps).catch(() => []));
  }, [deps]);

  useEffect(() => {
    void load();
  }, [load, props.hidden]);

  const act = async (j: SwapJournal, work: () => Promise<void>) => {
    setBusy(j.titleId);
    setError(null);
    try {
      await work();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
      void load();
    }
  };

  const listed = pendingJournals(journals, props.shownTitle);
  if (props.hidden || !deps || listed.length === 0) return null;
  return (
    <>
      <SwapJournalsView
        journals={listed}
        busy={busy}
        onRollback={(j) => void act(j, () => rollbackSwap(j, deps))}
        onFinish={(j, choice) => void act(j, () => finishSwap(j, choice, deps))}
      />
      {error && (
        <Callout tone="error" title={tr("fpkg.error", undefined, "Conversion error")}>
          {error}
        </Callout>
      )}
    </>
  );
}
