import { X } from "lucide-react";

import { Input, Select, Textarea } from "../../components";
import { useTr } from "../../state/lang";
import { useRosterStore } from "../../state/roster";
import { hostOf } from "../../lib/addr";
import { formatLine } from "../../lib/reportTimeline";
import type { Doing, ReportForm } from "../../lib/reportOutputs";
import Chips from "./Chips";

/** Which console, what the user was doing, what went wrong, and how to make it happen again. */
export default function WhatSection({
  form: f,
  updateForm,
}: {
  form: ReportForm;
  updateForm: (p: Partial<ReportForm>) => void;
}) {
  const tr = useTr();
  const profiles = useRosterStore((s) => s.profiles);

  const doingOptions: { value: Doing; label: string }[] = [
    { value: "connecting", label: tr("br_doing_connecting", undefined, "Connecting") },
    { value: "uploading", label: tr("br_doing_uploading", undefined, "Uploading") },
    { value: "installing", label: tr("br_doing_installing", undefined, "Installing") },
    { value: "browsing", label: tr("br_doing_browsing", undefined, "Browsing files") },
    { value: "launching", label: tr("br_doing_launching", undefined, "Launching a game") },
    { value: "payloads", label: tr("br_doing_payloads", undefined, "Payloads") },
    { value: "other", label: tr("br_doing_other", undefined, "Other") },
  ];

  return (
    <div className="grid gap-5">
      {/* Opened from an error's notification: that error leads the report. */}
      {f.pinned.length > 0 && (
        <div className="grid gap-1.5" data-testid="br-pinned">
          <span className="text-xs font-medium">{tr("br_about_event", undefined, "About this error")}</span>
          {f.pinned.map((e) => (
            <div
              key={`${e.src}-${e.ts}-${e.code}`}
              className="flex items-start gap-2 rounded-[var(--radius-card)] border border-[var(--color-border)] bg-[var(--color-surface)] px-3 py-2"
            >
              <span className="min-w-0 flex-1 break-words font-mono text-xs">{formatLine(e)}</span>
              <button
                type="button"
                aria-label={tr("br_remove", undefined, "Remove")}
                onClick={() => updateForm({ pinned: f.pinned.filter((p) => p !== e) })}
                className="shrink-0 rounded-full p-1 text-[var(--color-muted)] hover:text-[var(--color-text)]"
              >
                <X size={14} />
              </button>
            </div>
          ))}
        </div>
      )}
      <Select
        label={tr("br_console", undefined, "Which console?")}
        value={f.console ?? ""}
        onChange={(e) => updateForm({ console: e.target.value || null })}
        options={[
          ...profiles.map((p) => {
            const h = hostOf(p.host);
            // A name that already says the address ("PS5 (192.168.0.5)") is not repeated.
            const label = !p.name ? h : p.name.includes(h) ? p.name : `${p.name} (${h})`;
            return { value: h, label };
          }),
          { value: "", label: tr("br_console_none", undefined, "Not console-related") },
        ]}
      />
      <div className="grid gap-2">
        <Chips
          label={tr("br_doing", undefined, "What were you doing?")}
          value={f.doing}
          options={doingOptions}
          onChange={(doing) => updateForm({ doing })}
        />
        {f.doing === "other" && (
          <Input
            aria-label={tr("br_doing_other_label", undefined, "What were you doing?")}
            value={f.doingOther}
            placeholder={tr("br_doing_other_label", undefined, "What were you doing?")}
            onChange={(e) => updateForm({ doingOther: e.target.value })}
          />
        )}
      </div>
      <Textarea
        label={tr("br_what_went_wrong", undefined, "What went wrong?")}
        value={f.whatHappened}
        rows={4}
        onChange={(e) => updateForm({ whatHappened: e.target.value })}
        placeholder={tr("br_what_placeholder", undefined, "What you expected, and what happened instead.")}
      />
      <Textarea
        label={tr("br_steps_label", undefined, "Steps to reproduce (optional)")}
        value={f.steps}
        rows={3}
        placeholder={"1. …\n2. …"}
        onChange={(e) => updateForm({ steps: e.target.value })}
      />
      <div className="grid gap-4 sm:grid-cols-2">
        <Select
          label={tr("br_frequency", undefined, "How often?")}
          value={f.frequency}
          onChange={(e) => updateForm({ frequency: e.target.value as ReportForm["frequency"] })}
          options={[
            { value: "", label: "—" },
            { value: "once", label: tr("br_freq_once", undefined, "Once") },
            { value: "sometimes", label: tr("br_freq_sometimes", undefined, "Sometimes") },
            { value: "every_time", label: tr("br_freq_every", undefined, "Every time") },
          ]}
        />
        <Select
          label={tr("br_started", undefined, "When did it start?")}
          value={f.started}
          onChange={(e) => updateForm({ started: e.target.value as ReportForm["started"] })}
          options={[
            { value: "", label: "—" },
            { value: "after_app_update", label: tr("br_started_app", undefined, "After updating the app") },
            { value: "after_fw_update", label: tr("br_started_fw", undefined, "After a firmware update") },
            { value: "always", label: tr("br_started_always", undefined, "Always") },
            { value: "unknown", label: tr("br_started_unknown", undefined, "Don't know") },
          ]}
        />
      </div>
    </div>
  );
}
