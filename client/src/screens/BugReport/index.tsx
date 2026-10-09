import { useEffect, useMemo, useState } from "react";
import { useSearchParams } from "react-router";
import { Bug } from "lucide-react";

import { PageHeader, Button } from "../../components";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { hostOf } from "../../lib/addr";
import { isTauriEnv } from "../../lib/tauriEnv";
import { invoke } from "../../lib/invokeLogged";
import {
  canAdvance,
  clearDraft,
  defaultDraft,
  detectPlatform,
  loadDraft,
  platformFromHost,
  saveDraft,
  type Step,
  type WizardDraft,
} from "./draft";
import StepWhat from "./StepWhat";
import StepDetails, { type Attached } from "./StepDetails";
import StepInclude from "./StepInclude";
import StepSend from "./StepSend";

/**
 * Report a problem: a four-step wizard (spec §2). What happened → Details → What to include →
 * Send (build the zip, then Post to GitHub · Post to Discord · Download zip). The answers are
 * kept per console until the report is sent or discarded.
 */
export default function BugReportScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const consoleKey = hostOf(host) || "none";
  const [params] = useSearchParams();
  const pinTs = Number(params.get("event")) || null;
  const [appVersion, setAppVersion] = useState("");
  const [draft, setDraft] = useState<WizardDraft>(() => {
    const d = loadDraft(consoleKey) ?? defaultDraft("", detectPlatform(isTauriEnv()));
    if (d.form.console === null && host.trim()) d.form.console = hostOf(host);
    return d;
  });
  const [attached, setAttached] = useState<Attached>({ shots: [], files: [] });

  useEffect(() => {
    void import("../../lib/appVersion").then(({ getAppVersion }) =>
      getAppVersion()
        .then((v) => setAppVersion(v))
        .catch(() => setAppVersion("unknown")),
    );
  }, []);
  useEffect(() => {
    if (appVersion && !draft.form.appVersion)
      setDraft((d) => ({ ...d, form: { ...d.form, appVersion } }));
  }, [appVersion, draft.form.appVersion]);
  useEffect(() => saveDraft(consoleKey, draft), [consoleKey, draft]);
  // The desktop app knows its real OS and CPU (an Intel Mac reads as Apple Silicon to the web
  // view); it replaces the guess, unless the user has already chosen.
  useEffect(() => {
    if (!isTauriEnv()) return;
    void invoke<{ os: string; arch: string }>("host_platform")
      .then((h) => {
        const real = platformFromHost(h);
        if (real)
          setDraft((d) =>
            d.form.platform === detectPlatform(true) ? { ...d, form: { ...d.form, platform: real } } : d,
          );
      })
      .catch(() => {});
  }, []);

  const update = (patch: Partial<WizardDraft>) => setDraft((d) => ({ ...d, ...patch }));
  const updateForm = (patch: Partial<WizardDraft["form"]>) =>
    setDraft((d) => ({ ...d, form: { ...d.form, ...patch } }));
  const go = (step: Step) => update({ step });
  const discard = () => {
    clearDraft(consoleKey);
    setDraft(defaultDraft(appVersion, detectPlatform(isTauriEnv())));
    setAttached({ shots: [], files: [] });
  };

  const steps = useMemo(
    () => [
      tr("br_step_what", undefined, "What happened"),
      tr("br_step_details", undefined, "Details"),
      tr("br_step_include", undefined, "What to include"),
      tr("br_step_send", undefined, "Send"),
    ],
    [tr],
  );

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <PageHeader
        icon={Bug}
        title={tr("bug_report_page_title", undefined, "Bug report")}
        description={tr(
          "br_page_desc",
          undefined,
          "Tell us what went wrong. The report brings the app's, the engine's and the PS5 helper's own record of what happened, so you don't have to remember it.",
        )}
      />
      <ol className="mb-4 flex flex-wrap gap-2 text-xs" aria-label={tr("br_steps", undefined, "Steps")}>
        {steps.map((label, i) => {
          const n = (i + 1) as Step;
          const state = n === draft.step ? "current" : n < draft.step ? "done" : "todo";
          return (
            <li
              key={label}
              aria-current={state === "current" ? "step" : undefined}
              data-testid={`br-step-${n}`}
              className={
                "flex items-center gap-1.5 rounded-full border px-3 py-1 " +
                (state === "current"
                  ? "border-[var(--color-accent)] text-[var(--color-text)]"
                  : "border-[var(--color-border)] text-[var(--color-muted)]")
              }
            >
              <span className="font-semibold">{n}</span>
              {label}
            </li>
          );
        })}
      </ol>

      <div className="min-h-0 flex-1 overflow-y-auto pb-6">
        {draft.step === 1 && <StepWhat draft={draft} updateForm={updateForm} pinTs={pinTs} />}
        {draft.step === 2 && (
          <StepDetails draft={draft} updateForm={updateForm} attached={attached} setAttached={setAttached} />
        )}
        {draft.step === 3 && <StepInclude draft={draft} update={update} />}
        {draft.step === 4 && <StepSend draft={draft} updateForm={updateForm} attached={attached} onSent={() => clearDraft(consoleKey)} />}
      </div>

      {/* Back / Next stay in reach on a long step. */}
      <div className="sticky bottom-0 flex shrink-0 items-center gap-2 border-t border-[var(--color-border)] bg-[var(--color-surface)] py-3">
        <Button variant="ghost" size="sm" onClick={discard}>
          {tr("br_discard", undefined, "Start over")}
        </Button>
        <div className="ml-auto flex gap-2">
          {draft.step > 1 && (
            <Button variant="secondary" size="sm" onClick={() => go((draft.step - 1) as Step)}>
              {tr("br_back", undefined, "Back")}
            </Button>
          )}
          {draft.step < 4 && (
            <Button
              variant="primary"
              size="sm"
              disabled={!canAdvance(draft.step, draft)}
              onClick={() => go((draft.step + 1) as Step)}
            >
              {tr("br_next", undefined, "Next")}
            </Button>
          )}
        </div>
      </div>
    </div>
  );
}
