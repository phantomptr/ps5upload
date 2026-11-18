import { useMemo, useState } from "react";
import { Trophy } from "lucide-react";

import { Button } from "../../components";
import { useTr } from "../../state/lang";
import { profileTrophiesFix, profileTrophiesRestore } from "../../api/ps5";
import {
  applyTrophies,
  checkTrophies,
  undoTrophies,
  type TrophyRepairApi,
  type TrophyRepairStep,
} from "./trophyRepairFlow";

/** How many file paths the check shows before "and more". */
const SHOWN = 5;

/** Repair trophies for one user whose account has an ID (#853): a check that changes nothing,
 *  then the update, which the PS5 keeps a copy of so it can be undone. */
export function TrophyRepair({ addr, uid, slot }: { addr: string; uid: number; slot: number }) {
  const tr = useTr();
  const [step, setStep] = useState<TrophyRepairStep>({ kind: "idle" });
  const api: TrophyRepairApi = useMemo(
    () => ({
      fix: (dryRun) => profileTrophiesFix(uid, slot, dryRun, addr),
      restore: (backup) => profileTrophiesRestore(uid, backup, addr),
    }),
    [addr, uid, slot],
  );
  const busy = step.kind === "checking" || step.kind === "applying" || step.kind === "undoing";

  async function check() {
    setStep({ kind: "checking" });
    setStep(await checkTrophies(api));
  }
  async function apply() {
    setStep({ kind: "applying" });
    setStep(await applyTrophies(api));
  }
  async function undo(backup: string) {
    setStep({ kind: "undoing", backup });
    setStep(await undoTrophies(api, backup));
  }

  return (
    <div className="mt-2 text-xs">
      {(step.kind === "idle" || step.kind === "checking") && (
        <button
          type="button"
          className="inline-flex items-center gap-1 text-[var(--color-accent)] hover:underline disabled:opacity-50"
          disabled={busy}
          title={tr(
            "profile.trophies.hint",
            undefined,
            "If this user stopped getting trophies after the account ID changed, their trophy files may still have the old account.",
          )}
          onClick={() => void check()}
        >
          <Trophy size={12} />
          {step.kind === "checking"
            ? tr("profile.trophies.checking", undefined, "Checking trophy files…")
            : tr("profile.trophies.repair", undefined, "Repair trophies")}
        </button>
      )}
      {step.kind === "nothing" && (
        <p className="text-[var(--color-muted)]">
          {tr("profile.trophies.nothing", undefined, "Trophy files already match this account.")}
        </p>
      )}
      {step.kind === "preview" && (
        <div className="rounded-[var(--radius-card)] border border-[var(--color-border)] p-2">
          <p>
            {tr(
              "profile.trophies.preview",
              { n: step.count },
              `${step.count} trophy files have another account and will get this one. A copy of each is kept on the PS5.`,
            )}
          </p>
          <ul className="mt-1 font-mono text-[var(--color-muted)]">
            {step.files.slice(0, SHOWN).map((f) => (
              <li key={f.path} className="truncate" title={`${f.path} (${f.from})`}>
                {f.path.split("/").slice(-2, -1)[0] ?? f.path}
              </li>
            ))}
            {(step.more || step.files.length > SHOWN) && (
              <li>{tr("profile.trophies.more", undefined, "and more")}</li>
            )}
          </ul>
          <div className="mt-2 flex gap-2">
            <Button variant="secondary" size="sm" onClick={() => void apply()}>
              {tr("profile.trophies.apply", undefined, "Update trophy files")}
            </Button>
            <button
              type="button"
              className="text-[var(--color-muted)] hover:underline"
              onClick={() => setStep({ kind: "idle" })}
            >
              {tr("cancel", undefined, "Cancel")}
            </button>
          </div>
        </div>
      )}
      {step.kind === "applying" && (
        <p className="text-[var(--color-muted)]">
          {tr("profile.trophies.applying", undefined, "Updating trophy files…")}
        </p>
      )}
      {step.kind === "done" && (
        <p className={step.failed ? "text-[var(--color-warn)]" : "text-[var(--color-good)]"}>
          {tr("profile.trophies.updated", { n: step.changed }, `Trophy files updated: ${step.changed}.`)}{" "}
          {step.failed > 0 &&
            tr(
              "profile.trophies.failed",
              { n: step.failed },
              `${step.failed} could not be updated.`,
            )}{" "}
          {tr("profile.trophies.restart", undefined, "Restart the PS5 so it reads them.")}{" "}
          {step.backup && (
            <button
              type="button"
              className="text-[var(--color-accent)] hover:underline"
              onClick={() => void undo(step.backup)}
            >
              {tr("profile.trophies.undo", undefined, "Undo")}
            </button>
          )}
        </p>
      )}
      {step.kind === "undoing" && (
        <p className="text-[var(--color-muted)]">
          {tr("profile.trophies.undoing", undefined, "Putting the trophy files back…")}
        </p>
      )}
      {step.kind === "undone" && (
        <p className={step.failed ? "text-[var(--color-warn)]" : "text-[var(--color-good)]"}>
          {tr(
            "profile.trophies.undone",
            { n: step.restored },
            `Trophy files put back: ${step.restored}.`,
          )}{" "}
          {step.failed > 0 &&
            tr("profile.trophies.failed", { n: step.failed }, `${step.failed} could not be updated.`)}{" "}
          {step.backup && <UndoLink onClick={() => void undo(step.backup!)} />}
        </p>
      )}
      {step.kind === "error" && (
        <p className="text-[var(--color-bad)]">
          {step.gameRunning
            ? tr(
                "profile.trophies.game_running",
                undefined,
                "Close the game on the PS5 first, then try again.",
              )
            : step.message}{" "}
          {step.backup ? (
            // An undo that failed: the copy is still on the PS5, so Undo again, not Back (which
            // led to a check that finds nothing to change, and no undo).
            <UndoLink onClick={() => void undo(step.backup!)} />
          ) : (
            <button
              type="button"
              className="text-[var(--color-accent)] hover:underline"
              onClick={() => setStep({ kind: "idle" })}
            >
              {tr("profile.trophies.back", undefined, "Back")}
            </button>
          )}
        </p>
      )}
    </div>
  );
}

function UndoLink({ onClick }: { onClick: () => void }) {
  const tr = useTr();
  return (
    <button type="button" className="text-[var(--color-accent)] hover:underline" onClick={onClick}>
      {tr("profile.trophies.undo", undefined, "Undo")}
    </button>
  );
}
