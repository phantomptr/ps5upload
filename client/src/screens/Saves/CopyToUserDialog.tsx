// "Copy to another user…" (a save) and "Restore to another user…" (a backup) (#612): pick the
// user, confirm, wait, done. The engine does the work in one call; this only walks through it.
import { useState } from "react";
import { Check, UserRound } from "lucide-react";

import { Button, Modal } from "../../components";
import { useTr, type Translator } from "../../state/lang";
import type { CopyTarget } from "./copyTargets";
import { userHex } from "./saveUsers";

export type CopyState =
  | { step: "pick" }
  | { step: "confirm"; target: CopyTarget }
  | { step: "busy"; target: CopyTarget }
  | { step: "done"; target: CopyTarget }
  | { step: "failed"; target: CopyTarget; error: string };

export type CopyEvent =
  | { type: "pick"; target: CopyTarget }
  | { type: "back" }
  | { type: "start" }
  | { type: "done" }
  | { type: "failed"; error: string };

/** The dialog's next step. A user without a save of the game can't be picked. */
export function copyStep(s: CopyState, e: CopyEvent): CopyState {
  switch (e.type) {
    case "pick":
      return s.step === "pick" && e.target.save ? { step: "confirm", target: e.target } : s;
    case "back":
      return s.step === "confirm" || s.step === "failed" ? { step: "pick" } : s;
    case "start":
      return s.step === "confirm" ? { step: "busy", target: s.target } : s;
    case "done":
      return s.step === "busy" ? { step: "done", target: s.target } : s;
    case "failed":
      return s.step === "busy" ? { step: "failed", target: s.target, error: e.error } : s;
  }
}

/** What is copied: a user's save, or a backup (named by when it was made). */
export type CopySource = { kind: "save"; owner: string } | { kind: "backup"; when: string };

export interface CopyViewProps {
  game: string;
  source: CopySource;
  targets: CopyTarget[];
  state: CopyState;
  onPick: (t: CopyTarget) => void;
  onConfirm: () => void;
  onBack: () => void;
  onClose: () => void;
}

export function targetName(t: CopyTarget, tr: Translator): string {
  return t.name ?? tr("saves_user_other", { id: userHex(t.uid) }, "Other user ({id})");
}

/** The dialog's content for one step (no state of its own). */
export function CopyToUserView({ game, source, targets, state, onPick, onConfirm, onBack, onClose }: CopyViewProps) {
  const tr = useTr();
  const restore = source.kind === "backup";
  if (state.step === "pick") {
    return (
      <div className="flex flex-col gap-3" data-testid="copy-user-picker">
        <p className="text-sm">
          {source.kind === "save"
            ? tr("saves_copy_pick", { source: source.owner, game }, "Copy {source}'s save of {game} to:")
            : tr("save_backup_restore_pick", { when: source.when, game }, "Put the backup from {when} into whose save of {game}?")}
        </p>
        <ul className="flex flex-col gap-1">
          {targets.map((t) => {
            const name = targetName(t, tr);
            return (
              <li key={t.uid}>
                <button
                  type="button"
                  disabled={!t.save}
                  data-testid="copy-user-target"
                  data-has-save={t.save ? "true" : "false"}
                  onClick={() => onPick(t)}
                  className="flex w-full items-start gap-2 rounded-[var(--radius-field)] border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2 text-left text-sm enabled:hover:bg-[var(--color-surface-raised)] disabled:cursor-not-allowed disabled:opacity-60"
                >
                  <UserRound size={14} className="mt-0.5 shrink-0" aria-hidden />
                  <span className="min-w-0 flex-1">
                    <span className="block font-medium">{name}</span>
                    <span className="block text-xs text-[var(--color-muted)]">
                      {t.save
                        ? tr("saves_copy_has_save", undefined, "Has a save of this game")
                        : tr("saves_copy_no_save", { game, name }, "Start {game} once as {name} and save, then come back.")}
                    </span>
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
        <div className="flex justify-end">
          <Button variant="ghost" size="sm" onClick={onClose}>
            {tr("saves_copy_cancel", undefined, "Cancel")}
          </Button>
        </div>
      </div>
    );
  }
  const target = targetName(state.target, tr);
  if (state.step === "confirm") {
    return (
      <div className="flex flex-col gap-3" data-testid="copy-user-confirm">
        <p className="text-sm">
          {source.kind === "save"
            ? tr(
                "saves_copy_confirm",
                { target, game, source: source.owner },
                "Replace {target}'s save of {game} with {source}'s? {target}'s current save is backed up first.",
              )
            : tr(
                "save_backup_restore_to_user_confirm",
                { target, game, when: source.when },
                "Replace {target}'s save of {game} with the backup from {when}? {target}'s current save is backed up first.",
              )}
        </p>
        <div className="flex flex-wrap justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={onBack}>
            {tr("saves_copy_back", undefined, "Back")}
          </Button>
          <Button variant="danger" size="sm" onClick={onConfirm} data-testid="copy-user-replace">
            {tr("saves_copy_replace", undefined, "Replace save")}
          </Button>
        </div>
      </div>
    );
  }
  if (state.step === "busy") {
    return (
      <p className="text-sm" role="status" data-testid="copy-user-busy">
        {restore
          ? tr("save_backup_restore_to_user_busy", undefined, "Restoring the backup. Keep the app open.")
          : tr("saves_copy_busy", undefined, "Copying the save. Keep the app open.")}
      </p>
    );
  }
  if (state.step === "done") {
    return (
      <div className="flex flex-col gap-3" data-testid="copy-user-done">
        <p className="flex items-start gap-2 text-sm">
          <Check size={14} className="mt-0.5 shrink-0 text-[var(--color-good)]" aria-hidden />
          <span>
            {source.kind === "save"
              ? tr(
                  "saves_copy_done",
                  { game, target, source: source.owner },
                  "{game} for {target} now has {source}'s save. Start the game as {target} to load it.",
                )
              : tr(
                  "save_backup_restore_to_user_done",
                  { game, target, when: source.when },
                  "{game} for {target} now has the backup from {when}. Start the game as {target} to load it.",
                )}
          </span>
        </p>
        <div className="flex justify-end">
          <Button variant="primary" size="sm" onClick={onClose}>
            {tr("saves_copy_close", undefined, "Close")}
          </Button>
        </div>
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-3" data-testid="copy-user-failed">
      <p className="text-sm text-[var(--color-bad)]" role="alert">
        {restore
          ? tr("save_backup_restore_to_user_failed", { error: state.error }, "Couldn't restore the backup: {error}")
          : tr("saves_copy_failed", { error: state.error }, "Couldn't copy the save: {error}")}
      </p>
      <div className="flex flex-wrap justify-end gap-2">
        <Button variant="ghost" size="sm" onClick={onBack}>
          {tr("saves_copy_back", undefined, "Back")}
        </Button>
        <Button variant="secondary" size="sm" onClick={onClose}>
          {tr("saves_copy_close", undefined, "Close")}
        </Button>
      </div>
    </div>
  );
}

/** The dialog: `run` does the copy to the user picked; `onDone` runs once it worked. */
export default function CopyToUserDialog({
  game,
  source,
  targets,
  run,
  onClose,
  onDone,
}: {
  game: string;
  source: CopySource;
  targets: CopyTarget[];
  run: (t: CopyTarget) => Promise<void>;
  onClose: () => void;
  onDone?: () => void;
}) {
  const tr = useTr();
  const [state, setState] = useState<CopyState>({ step: "pick" });
  const go = (e: CopyEvent) => setState((s) => copyStep(s, e));
  const confirm = async () => {
    if (state.step !== "confirm") return;
    const target = state.target;
    go({ type: "start" });
    try {
      await run(target);
      go({ type: "done" });
      onDone?.();
    } catch (e) {
      go({ type: "failed", error: e instanceof Error ? e.message : String(e) });
    }
  };
  return (
    <Modal
      open
      // Not while it runs: closing would hide a copy still going.
      onClose={() => state.step !== "busy" && onClose()}
      closeOnScrim={state.step !== "busy"}
      title={
        source.kind === "save"
          ? tr("saves_copy_title", undefined, "Copy to another user")
          : tr("save_backup_restore_to_user_title", undefined, "Restore to another user")
      }
      size="md"
      bodyClassName="p-4 sm:p-5"
    >
      <CopyToUserView
        game={game}
        source={source}
        targets={targets}
        state={state}
        onPick={(t) => go({ type: "pick", target: t })}
        onConfirm={() => void confirm()}
        onBack={() => go({ type: "back" })}
        onClose={onClose}
      />
    </Modal>
  );
}
