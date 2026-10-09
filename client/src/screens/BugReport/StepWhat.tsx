import { useEffect, useState } from "react";

import { Select, Textarea, Checkbox, Input } from "../../components";
import { useTr } from "../../state/lang";
import { useRosterStore } from "../../state/roster";
import { hostOf } from "../../lib/addr";
import type { EventRecord } from "../../lib/eventRecord";
import type { Doing } from "../../lib/reportOutputs";
import { fetchTimeline, formatLine, recentProblems } from "../../lib/reportTimeline";
import { MIN_DESCRIPTION, type WizardDraft } from "./draft";

const sameEvent = (a: EventRecord, b: EventRecord) => a.ts === b.ts && a.src === b.src && a.code === b.code;

/** Step 1: which console, what the user was doing, what went wrong, and which of the problems
 *  the app recorded this is about (those are pinned at the top of the report). */
export default function StepWhat({
  draft,
  updateForm,
  pinTs,
}: {
  draft: WizardDraft;
  updateForm: (p: Partial<WizardDraft["form"]>) => void;
  /** An event to pre-tick: the report was opened from that error's notification. */
  pinTs: number | null;
}) {
  const tr = useTr();
  const profiles = useRosterStore((s) => s.profiles);
  const [problems, setProblems] = useState<EventRecord[] | null>(null);
  const f = draft.form;

  useEffect(() => {
    let alive = true;
    const now = Date.now();
    void fetchTimeline(now - 86_400_000, now).then((t) => {
      if (!alive) return;
      const list = recentProblems(t.events, 10);
      setProblems(list);
      if (pinTs) {
        const hit = t.events.find((e) => e.ts === pinTs);
        if (hit && !f.pinned.some((p) => sameEvent(p, hit))) updateForm({ pinned: [...f.pinned, hit] });
      }
    });
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const doingOptions: { value: Doing; label: string }[] = [
    { value: "connecting", label: tr("br_doing_connecting", undefined, "Connecting") },
    { value: "uploading", label: tr("br_doing_uploading", undefined, "Uploading") },
    { value: "installing", label: tr("br_doing_installing", undefined, "Installing") },
    { value: "browsing", label: tr("br_doing_browsing", undefined, "Browsing files") },
    { value: "launching", label: tr("br_doing_launching", undefined, "Launching a game") },
    { value: "payloads", label: tr("br_doing_payloads", undefined, "Payloads") },
    { value: "other", label: tr("br_doing_other", undefined, "Other") },
  ];
  const togglePin = (e: EventRecord) =>
    updateForm({
      pinned: f.pinned.some((p) => sameEvent(p, e)) ? f.pinned.filter((p) => !sameEvent(p, e)) : [...f.pinned, e],
    });
  const chars = f.whatHappened.trim().length;

  return (
    <div className="grid max-w-3xl gap-4">
      <Select
        label={tr("br_console", undefined, "Which console?")}
        value={f.console ?? ""}
        onChange={(e) => updateForm({ console: e.target.value || null })}
        options={[
          ...profiles.map((p) => ({ value: hostOf(p.host), label: p.name ? `${p.name} (${hostOf(p.host)})` : hostOf(p.host) })),
          { value: "", label: tr("br_console_none", undefined, "Not console-related") },
        ]}
      />
      <fieldset>
        <legend className="mb-1.5 text-xs font-medium uppercase tracking-wide text-[var(--color-muted)]">
          {tr("br_doing", undefined, "What were you doing?")}
        </legend>
        <div className="flex flex-wrap gap-2" role="radiogroup">
          {doingOptions.map((o) => (
            <button
              key={o.value}
              type="button"
              role="radio"
              aria-checked={f.doing === o.value}
              onClick={() => updateForm({ doing: o.value })}
              className={
                "rounded-md border px-3 py-1.5 text-sm " +
                (f.doing === o.value
                  ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)]"
                  : "border-[var(--color-border)] hover:bg-[var(--color-surface-3)]")
              }
            >
              {o.label}
            </button>
          ))}
        </div>
        {f.doing === "other" && (
          <div className="mt-2">
            <Input
              label={tr("br_doing_other_label", undefined, "What were you doing?")}
              value={f.doingOther}
              onChange={(e) => updateForm({ doingOther: e.target.value })}
            />
          </div>
        )}
      </fieldset>
      <Textarea
        label={tr("br_what_went_wrong", undefined, "What went wrong?")}
        value={f.whatHappened}
        rows={5}
        onChange={(e) => updateForm({ whatHappened: e.target.value })}
        placeholder={tr("br_what_placeholder", undefined, "What you expected, and what happened instead.")}
        hint={
          chars < MIN_DESCRIPTION
            ? tr("br_min_chars", { n: MIN_DESCRIPTION - chars }, `${MIN_DESCRIPTION - chars} more characters, please.`)
            : undefined
        }
      />
      <section>
        <h3 className="mb-1 text-sm font-medium">{tr("br_problems_title", undefined, "Recent problems we noticed")}</h3>
        <p className="mb-2 text-xs text-[var(--color-muted)]">
          {tr("br_problems_hint", undefined, "Tick the ones this report is about. They go at the top of the report.")}
        </p>
        {problems === null ? (
          <p className="text-xs text-[var(--color-muted)]">{tr("br_loading", undefined, "Loading…")}</p>
        ) : problems.length === 0 ? (
          <p className="text-xs text-[var(--color-muted)]">{tr("br_problems_none", undefined, "Nothing recorded in the last 24 hours.")}</p>
        ) : (
          <ul className="grid gap-1" data-testid="br-problems">
            {problems.map((e) => (
              <li key={`${e.src}-${e.ts}-${e.code}`}>
                <Checkbox
                  checked={f.pinned.some((p) => sameEvent(p, e))}
                  onChange={() => togglePin(e)}
                  label={<span className="font-mono text-xs">{formatLine(e)}</span>}
                />
              </li>
            ))}
          </ul>
        )}
      </section>
    </div>
  );
}
