import { useState } from "react";
import { CheckCircle2, Copy, Download, ExternalLink, FolderOpen, MessageSquare } from "lucide-react";

import { Button, Checkbox, Input } from "../../components";
import { openLocalPath } from "../../lib/openLocalPath";
import { isTauriEnv } from "../../lib/tauriEnv";
import { useTr } from "../../state/lang";
import type { BuiltReport } from "../../lib/reportBuilder";
import type { SavedReport } from "../../lib/saveReport";
import { publicOutputs, type ReportForm } from "../../lib/reportOutputs";
import { DISCORD_REPORT_URL } from "../../lib/reportProblem";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { writeClipboard } from "../../lib/clipboard";

/** The saved zip's file name, from a path with either separator. */
export function fileNameOf(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

/** The report is saved: Post to GitHub · Post to Discord · Download zip. */
export default function SendPanel({
  form,
  updateForm,
  built,
  saved,
  onDownload,
}: {
  form: ReportForm;
  updateForm: (p: Partial<ReportForm>) => void;
  built: BuiltReport;
  saved: SavedReport;
  onDownload: () => void;
}) {
  const tr = useTr();
  const [discordFallback, setDiscordFallback] = useState<string | null>(null);
  // A link opens the issue form but can't put a file in it: issues came in saying "the attached
  // zip" with none. Posting asks first that the zip will be dragged in.
  const [willAttach, setWillAttach] = useState(false);
  const [revealFailed, setRevealFailed] = useState(false);
  const desktop = isTauriEnv();
  const zipName = fileNameOf(saved.dest);

  // Redacted like the zip: an issue is public.
  const outputs = () => publicOutputs(form, built.problemLines, true);
  const postGithub = () => void openExternalUrl(outputs().githubUrl);
  const showZip = () => {
    setRevealFailed(false);
    void openLocalPath(saved.dest).then((ok) => setRevealFailed(!ok));
  };
  const postDiscord = async () => {
    const text = outputs().discordText;
    // Copy first: a page that lost focus to the browser can't write the clipboard.
    if (await writeClipboard(text)) setDiscordFallback(null);
    else setDiscordFallback(text);
    void openExternalUrl(DISCORD_REPORT_URL);
  };

  return (
    <div className="grid gap-4">
      <div className="flex items-start gap-2.5" data-testid="br-saved">
        <CheckCircle2 size={18} className="mt-0.5 shrink-0 text-[var(--color-good)]" />
        <div className="min-w-0">
          <div className="text-sm font-medium">{tr("br_saved", undefined, "Report saved")}</div>
          <div className="break-all text-xs text-[var(--color-muted)]" data-testid="br-saved-path">
            {desktop
              ? saved.dest
              : tr("br_saved_browser", { name: zipName }, `${zipName}, in your browser's downloads`)}
          </div>
          {desktop && (
            <Button
              className="mt-1.5"
              size="sm"
              variant="secondary"
              leftIcon={<FolderOpen size={12} />}
              onClick={showZip}
              data-testid="br-show-zip"
            >
              {tr("br_show_zip", undefined, "Show in folder")}
            </Button>
          )}
          {revealFailed && (
            <div className="mt-1 text-xs text-[var(--color-warn)]">
              {tr("br_show_zip_failed", { path: saved.dest }, `Couldn't open the folder. The zip is at ${saved.dest}.`)}
            </div>
          )}
          {built.missing.length > 0 && (
            <div className="mt-1 text-xs text-[var(--color-warn)]">
              {tr("br_missing", { n: built.missing.length }, `${built.missing.length} things couldn't be read (see MISSING.txt in the zip).`)}
            </div>
          )}
        </div>
      </div>
      <div className="grid gap-3 sm:grid-cols-3">
        {[
          {
            key: "gh",
            button: (
              <>
                <Checkbox
                  checked={willAttach}
                  onChange={setWillAttach}
                  label={tr("br_will_attach", undefined, "I'll drag the zip into the issue before I submit it")}
                />
                <Button
                  className="w-full"
                  variant="primary"
                  leftIcon={<ExternalLink size={14} />}
                  onClick={postGithub}
                  disabled={!willAttach}
                  data-testid="br-post-github"
                >
                  {tr("br_post_github", undefined, "Post to GitHub")}
                </Button>
              </>
            ),
            hint: tr(
              "br_post_github_hint_v2",
              { name: zipName },
              `The app can't attach the zip for you. In the issue, drag ${zipName} into the "Bug report zip" box, wait for its link to appear, then Submit.`,
            ),
          },
          {
            key: "dc",
            button: (
              <Button className="w-full" variant="secondary" leftIcon={<MessageSquare size={14} />} onClick={() => void postDiscord()}>
                {tr("br_post_discord", undefined, "Post to Discord")}
              </Button>
            ),
            hint: tr(
              "br_post_discord_hint_v2",
              { name: zipName },
              `The text is copied: paste it (Ctrl/⌘+V), then drag in ${zipName} before you send.`,
            ),
          },
          {
            key: "zip",
            button: (
              <Button className="w-full" variant="secondary" leftIcon={<Download size={14} />} onClick={onDownload}>
                {tr("br_download", undefined, "Download zip")}
              </Button>
            ),
            hint: "",
          },
        ].map((a) => (
          <div key={a.key} className="grid content-start gap-1.5">
            {a.button}
            {a.hint && <p className="text-xs text-[var(--color-muted)]">{a.hint}</p>}
          </div>
        ))}
      </div>
      {discordFallback !== null && (
        <div className="grid gap-2">
          <p className="text-xs">{tr("br_copy_fallback", undefined, "Copying isn't allowed here. Select the text and copy it:")}</p>
          <textarea
            readOnly
            value={discordFallback}
            aria-label={tr("br_copy_fallback_label", undefined, "Report text to copy")}
            rows={8}
            onFocus={(e) => e.currentTarget.select()}
            className="w-full rounded-[var(--radius-field)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] p-2 font-mono text-xs"
          />
          <div>
            <Button size="sm" variant="secondary" leftIcon={<Copy size={12} />} onClick={() => void writeClipboard(discordFallback)}>
              {tr("br_copy", undefined, "Copy")}
            </Button>
          </div>
        </div>
      )}
      <Input
        label={tr("br_issue_link", undefined, "GitHub issue link (optional, added to the Discord text)")}
        value={form.githubIssueUrl}
        placeholder={tr("br_issue_link_placeholder", undefined, "https://github.com/phantomptr/ps5upload/issues/…")}
        onChange={(e) => updateForm({ githubIssueUrl: e.target.value })}
      />
    </div>
  );
}
