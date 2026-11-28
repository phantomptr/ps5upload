import { useState } from "react";
import { AlertTriangle, CheckCircle2, HelpCircle, RefreshCw, ShieldCheck, Stethoscope } from "lucide-react";

import { fsChmod } from "../../api/ps5";
import { gameDoctor, type DoctorAnswer, type DoctorCheck } from "../../api/gameDoctor";
import { Button, Card } from "../../components";
import { transferAddr } from "../../lib/addr";
import { humanizePs5Error } from "../../lib/humanizeError";
import { useTr } from "../../state/lang";
import { pushNotification } from "../../state/notifications";
import { withConsolePrefix } from "../../state/roster";

type Tr = ReturnType<typeof useTr>;

/** A check's name, as the list shows it. */
export function doctorCheckName(tr: Tr, id: DoctorCheck["id"]): string {
  switch (id) {
    case "fake_package":
      return tr("doctor_name_fake_package", undefined, "Fake package support (kstuff)");
    case "mount_layer":
      return tr("doctor_name_mount_layer", undefined, "Mounting (ShadowMount+)");
    case "package_size":
      return tr("doctor_name_package_size", undefined, "Package size");
    case "database":
      return tr("doctor_name_database", undefined, "In the PS5's game list");
    case "permissions":
      return tr("doctor_name_permissions", undefined, "File permissions");
    case "free_space":
      return tr("doctor_name_free_space", undefined, "Free space");
  }
}

/** What a check found, in the user's language; the engine's English when the app has no words
 *  for that reason. */
export function doctorCheckText(tr: Tr, c: DoctorCheck): string {
  return tr(`doctor_${c.id}_${c.reason}`, c.params, c.message || c.reason);
}

function StatusIcon({ status }: { status: DoctorCheck["status"] }) {
  if (status === "pass") return <CheckCircle2 size={15} className="mt-0.5 shrink-0 text-[var(--color-good)]" aria-hidden />;
  if (status === "problem") return <AlertTriangle size={15} className="mt-0.5 shrink-0 text-[var(--color-bad)]" aria-hidden />;
  return <HelpCircle size={15} className="mt-0.5 shrink-0 text-[var(--color-muted)]" aria-hidden />;
}

/**
 * "Something wrong?": one button that checks, in order, the usual reasons a game does not show
 * on the PS5's home screen or does not start, and says which one failed and what to do. Each
 * check is OK, a problem, or "can't tell" when the console didn't say enough.
 */
export function GameDoctorCard({ host, titleId, name }: { host: string; titleId: string; name: string }) {
  const tr = useTr();
  const [answer, setAnswer] = useState<DoctorAnswer | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<"check" | "fix" | null>(null);

  const run = async () => {
    setBusy("check");
    setError(null);
    try {
      setAnswer(await gameDoctor(host, titleId));
    } catch (e) {
      setAnswer(null);
      setError(humanizePs5Error(e instanceof Error ? e.message : String(e)));
    } finally {
      setBusy(null);
    }
  };

  const fixPermissions = async (source: string) => {
    setBusy("fix");
    try {
      await fsChmod(transferAddr(host), source, "0777", true);
      pushNotification("success", withConsolePrefix(host, name), {
        body: tr(
          "installed_fix_permissions_done",
          undefined,
          "Permissions fixed: every file in this game's folder is now 0777. Press Play.",
        ),
      });
    } catch (e) {
      pushNotification("error", withConsolePrefix(host, name), {
        body: humanizePs5Error(e instanceof Error ? e.message : String(e)),
      });
      setBusy(null);
      return;
    }
    setBusy(null);
    await run();
  };

  const first = answer?.firstProblem ? answer.checks.find((c) => c.id === answer.firstProblem) : undefined;
  const unknowns = answer ? answer.checks.filter((c) => c.status === "unknown").length : 0;

  return (
    <Card>
      <h2 className="mb-2 flex items-center gap-3 text-[1.0625rem] font-semibold tracking-[-0.01em]">
        <span className="grid h-8 w-8 shrink-0 place-items-center rounded-full bg-[var(--color-accent-soft)] text-[var(--color-accent-bright)]">
          <Stethoscope size={15} aria-hidden />
        </span>
        {tr("doctor_title", undefined, "Something wrong?")}
      </h2>
      <p className="mb-3 text-xs text-[var(--color-muted)]">
        {tr(
          "doctor_intro",
          undefined,
          "If this game doesn't show on the PS5's home screen, or stays on its loading screen, check the usual reasons one by one. Nothing on the PS5 is changed.",
        )}
      </p>
      {answer && (
        <div data-testid="game-doctor-result">
          <p className="mb-2 text-sm font-medium" role="status">
            {first
              ? tr("doctor_found", { name: doctorCheckName(tr, first.id) }, "Likely reason: {name}")
              : unknowns > 0
                ? tr(
                    "doctor_none_found_unknown",
                    undefined,
                    "No problem found. A check marked “Can't tell” could still be the reason.",
                  )
                : tr("doctor_none_found", undefined, "No problem found in these checks.")}
          </p>
          <ul className="mb-3 grid gap-2">
            {answer.checks.map((c) => (
              <li
                key={c.id}
                className="flex gap-2 rounded-md border border-[var(--color-border)] px-3 py-2"
                data-testid={`doctor-${c.id}`}
                data-status={c.status}
              >
                <StatusIcon status={c.status} />
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-baseline gap-x-2 text-sm">
                    <span className="font-medium">{doctorCheckName(tr, c.id)}</span>
                    <span
                      className={
                        c.status === "problem"
                          ? "text-xs text-[var(--color-bad)]"
                          : "text-xs text-[var(--color-muted)]"
                      }
                    >
                      {c.status === "pass"
                        ? tr("doctor_status_pass", undefined, "OK")
                        : c.status === "problem"
                          ? tr("doctor_status_problem", undefined, "Problem")
                          : tr("doctor_status_unknown", undefined, "Can't tell")}
                    </span>
                  </div>
                  <p className="text-xs text-[var(--color-muted)]">{doctorCheckText(tr, c)}</p>
                  {c.fix === "fix_permissions" && answer.source && (
                    <Button
                      className="mt-2"
                      size="sm"
                      variant="secondary"
                      leftIcon={<ShieldCheck size={13} />}
                      loading={busy === "fix"}
                      disabled={busy !== null}
                      onClick={() => void fixPermissions(answer.source as string)}
                    >
                      {tr("installed_fix_permissions_short", undefined, "Fix permissions")}
                    </Button>
                  )}
                </div>
              </li>
            ))}
          </ul>
        </div>
      )}
      {error && (
        <p className="mb-3 text-xs text-[var(--color-bad)]" role="alert">
          {tr("doctor_failed", { error }, "Couldn't check: {error}")}
        </p>
      )}
      <Button
        size="sm"
        variant={answer ? "ghost" : "secondary"}
        leftIcon={answer ? <RefreshCw size={13} /> : <Stethoscope size={13} />}
        loading={busy === "check"}
        disabled={busy !== null}
        onClick={() => void run()}
        data-testid="game-doctor-run"
      >
        {answer ? tr("doctor_again", undefined, "Check again") : tr("doctor_run", undefined, "Check")}
      </Button>
    </Card>
  );
}
