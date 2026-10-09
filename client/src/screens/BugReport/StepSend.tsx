import { useState } from "react";
import { CheckCircle2, Copy, Download, ExternalLink, FileArchive, MessageSquare } from "lucide-react";

import { Button, ErrorCard, Input } from "../../components";
import { useTr } from "../../state/lang";
import { buildReport, type BuiltReport, type SourceId } from "../../lib/reportBuilder";
import { saveReport, type SavedReport } from "../../lib/saveReport";
import { discordText, githubIssueUrl } from "../../lib/reportOutputs";
import { DISCORD_REPORT_URL, GITHUB_ISSUES_URL } from "../../lib/reportProblem";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { writeClipboard } from "../../lib/clipboard";
import { isTauriEnv } from "../../lib/tauriEnv";
import { rangeStart, type WizardDraft } from "./draft";
import type { Attached } from "./StepDetails";

type Progress = Partial<Record<SourceId, "running" | "ok" | "missing">>;

/** Step 4: build and save the zip, then Post to GitHub · Post to Discord · Download zip. */
export default function StepSend({
  draft,
  updateForm,
  attached,
  onSent,
}: {
  draft: WizardDraft;
  updateForm: (p: Partial<WizardDraft["form"]>) => void;
  attached: Attached;
  onSent: () => void;
}) {
  const tr = useTr();
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<Progress>({});
  const [built, setBuilt] = useState<BuiltReport | null>(null);
  const [saved, setSaved] = useState<SavedReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [discordFallback, setDiscordFallback] = useState<string | null>(null);
  const [since] = useState(() => rangeStart(draft, Date.now()));

  const save = async (b: BuiltReport) => {
    const s = await saveReport(b, {
      redact: draft.redact,
      since,
      imagePaths: attached.shots.map((x) => x.path),
    });
    if (s) setSaved(s);
    return s;
  };

  const build = async () => {
    setBusy(true);
    setError(null);
    setProgress({});
    try {
      const b = await buildReport({
        form: draft.form,
        since,
        until: Date.now(),
        cats: new Set(draft.cats),
        sources: new Set(draft.sources),
        consoles: draft.form.console ? [draft.form.console] : [],
        images: attached.files,
        platform: isTauriEnv() ? "desktop" : "browser",
        onProgress: (id, state) => setProgress((p) => ({ ...p, [id]: state })),
      });
      setBuilt(b);
      if (await save(b)) onSent();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const postGithub = () => void openExternalUrl(githubIssueUrl(draft.form, built?.problemLines ?? []));
  const postDiscord = async () => {
    const text = discordText(draft.form);
    if (await writeClipboard(text)) setDiscordFallback(null);
    else setDiscordFallback(text);
    void openExternalUrl(DISCORD_REPORT_URL);
  };

  return (
    <div className="grid max-w-3xl gap-4">
      {!saved && (
        <div>
          <Button variant="primary" leftIcon={<FileArchive size={14} />} loading={busy} disabled={busy} onClick={() => void build()}>
            {tr("br_build", undefined, "Build report")}
          </Button>
          {Object.keys(progress).length > 0 && (
            <ul className="mt-3 grid gap-0.5 text-xs" data-testid="br-progress">
              {Object.entries(progress).map(([id, state]) => (
                <li key={id} className={state === "missing" ? "text-[var(--color-warn)]" : "text-[var(--color-muted)]"}>
                  {state === "running" ? "…" : state === "ok" ? "✓" : "!"} {id}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
      {error && <ErrorCard title={tr("br_build_failed", undefined, "Couldn't build the report")} detail={error} />}

      {saved && built && (
        <>
          <div className="flex items-start gap-2 text-sm" data-testid="br-saved">
            <CheckCircle2 size={16} className="mt-0.5 shrink-0 text-[var(--color-good)]" />
            <div>
              <div className="font-medium">{tr("br_saved", undefined, "Report saved")}</div>
              <div className="break-all text-xs text-[var(--color-muted)]">{saved.dest}</div>
              {built.missing.length > 0 && (
                <div className="mt-1 text-xs text-[var(--color-warn)]">
                  {tr("br_missing", { n: built.missing.length }, `${built.missing.length} things couldn't be read (see MISSING.txt in the zip).`)}
                </div>
              )}
            </div>
          </div>
          <div className="grid gap-3 sm:grid-cols-3">
            <div>
              <Button className="w-full" variant="primary" leftIcon={<ExternalLink size={14} />} onClick={postGithub}>
                {tr("br_post_github", undefined, "Post to GitHub")}
              </Button>
              <p className="mt-1 text-xs text-[var(--color-muted)]">{tr("br_post_github_hint", undefined, "Drag the zip (just saved) into the issue, then Submit.")}</p>
            </div>
            <div>
              <Button className="w-full" variant="secondary" leftIcon={<MessageSquare size={14} />} onClick={() => void postDiscord()}>
                {tr("br_post_discord", undefined, "Post to Discord")}
              </Button>
              <p className="mt-1 text-xs text-[var(--color-muted)]">{tr("br_post_discord_hint", undefined, "The text is copied: paste it (Ctrl/⌘+V), then drag in the zip.")}</p>
            </div>
            <div>
              <Button className="w-full" variant="secondary" leftIcon={<Download size={14} />} onClick={() => void save(built)}>
                {tr("br_download", undefined, "Download zip")}
              </Button>
            </div>
          </div>
          {discordFallback !== null && (
            <div>
              <p className="mb-1 text-xs">{tr("br_copy_fallback", undefined, "Copying isn't allowed here. Select the text and copy it:")}</p>
              <textarea
                readOnly
                value={discordFallback}
                rows={8}
                onFocus={(e) => e.currentTarget.select()}
                className="w-full rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] p-2 font-mono text-xs"
              />
              <Button size="sm" variant="secondary" leftIcon={<Copy size={12} />} onClick={() => void writeClipboard(discordFallback)}>
                {tr("br_copy", undefined, "Copy")}
              </Button>
            </div>
          )}
          <Input
            label={tr("br_issue_link", undefined, "GitHub issue link (optional, added to the Discord text)")}
            value={draft.form.githubIssueUrl}
            placeholder={tr("br_issue_link_placeholder", undefined, "https://github.com/phantomptr/ps5upload/issues/…")}
            onChange={(e) => updateForm({ githubIssueUrl: e.target.value })}
          />
        </>
      )}
      <p className="text-xs text-[var(--color-muted)]">
        {tr("br_where", undefined, "Where to report:")}{" "}
        <a className="underline" href={GITHUB_ISSUES_URL} onClick={(e) => (e.preventDefault(), void openExternalUrl(GITHUB_ISSUES_URL))}>
          {tr("br_where_github", undefined, "GitHub issues")}
        </a>{" "}
        ·{" "}
        <a className="underline" href={DISCORD_REPORT_URL} onClick={(e) => (e.preventDefault(), void openExternalUrl(DISCORD_REPORT_URL))}>
          {tr("br_where_discord", undefined, "Discord #bugs-report")}
        </a>
      </p>
    </div>
  );
}
