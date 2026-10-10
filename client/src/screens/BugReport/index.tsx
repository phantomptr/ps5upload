import { useEffect, useRef, useState } from "react";
import { useSearchParams } from "react-router";
import { Bug, ExternalLink, FileArchive, Image as ImageIcon, MessageSquare, MessageSquareWarning, Monitor, ScrollText, Send } from "lucide-react";

import { PageHeader, Button, Card, ErrorCard } from "../../components";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { hostOf } from "../../lib/addr";
import { isTauriEnv } from "../../lib/tauriEnv";
import { invoke } from "../../lib/invokeLogged";
import { EVENT_CATS } from "../../lib/eventRecord";
import { buildReport, SOURCE_IDS, type BuiltReport } from "../../lib/reportBuilder";
import { saveReport, type SavedReport } from "../../lib/saveReport";
import { fetchTimeline } from "../../lib/reportTimeline";
import { DISCORD_REPORT_URL, GITHUB_ISSUES_URL } from "../../lib/reportProblem";
import { openExternalUrl } from "../../lib/openExternalUrl";
import { humanizePs5Error } from "../../lib/humanizeError";
import type { ReportForm } from "../../lib/reportOutputs";
import {
  clearDraft,
  defaultDraft,
  detectPlatform,
  loadDraft,
  platformFromHost,
  rangeStart,
  saveDraft,
  whatIsMissing,
  type ReportDraft,
} from "./draft";
import WhatSection from "./WhatSection";
import ScreenshotsSection, { type Attached } from "./ScreenshotsSection";
import SetupSection from "./SetupSection";
import LogsSection from "./LogsSection";
import SendPanel from "./SendPanel";

const noImages: Attached = { shots: [], files: [] };

/**
 * Report a problem on one page: what happened, screenshots, the setup (filled in), and how far
 * back the logs go. Create report gathers everything recorded in that window from the app, the
 * engine and the PS5, saves the zip, then offers Post to GitHub · Post to Discord · Download zip.
 * The answers are kept per console until the report is created or discarded.
 */
export default function BugReportScreen() {
  const host = useConnectionStore((s) => s.host);
  // Each console keeps its own draft: switching consoles loads that console's.
  return <BugReportPage key={hostOf(host) || "none"} host={host} />;
}

function BugReportPage({ host }: { host: string }) {
  const tr = useTr();
  const consoleKey = hostOf(host) || "none";
  const [params] = useSearchParams();
  const pinTs = Number(params.get("event")) || null;
  const [appVersion, setAppVersion] = useState("");
  const [draft, setDraft] = useState<ReportDraft>(() => {
    const d = loadDraft(consoleKey) ?? defaultDraft("", detectPlatform(isTauriEnv()));
    if (d.form.console === null && host.trim()) d.form.console = hostOf(host);
    return d;
  });
  const [attached, setAttached] = useState<Attached>(noImages);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<{ built: BuiltReport; saved: SavedReport; since: number } | null>(null);
  const top = useRef<HTMLDivElement>(null);

  useEffect(() => {
    void import("../../lib/appVersion").then(({ getAppVersion }) =>
      getAppVersion()
        .then((v) => setAppVersion(v))
        .catch(() => setAppVersion("unknown")),
    );
  }, []);
  useEffect(() => {
    if (appVersion && !draft.form.appVersion) setDraft((d) => ({ ...d, form: { ...d.form, appVersion } }));
  }, [appVersion, draft.form.appVersion]);
  // Once created, the report is done: editing its issue link must not bring the draft back.
  const created = result !== null;
  useEffect(() => {
    if (!created) saveDraft(consoleKey, draft);
  }, [consoleKey, draft, created]);
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
  // Opened from an error's notification: that error leads the report.
  useEffect(() => {
    if (!pinTs) return;
    void fetchTimeline(pinTs - 60_000, Date.now()).then((t) => {
      const hit = t.events.find((e) => e.ts === pinTs);
      if (hit)
        setDraft((d) =>
          d.form.pinned.some((p) => p.ts === hit.ts && p.code === hit.code)
            ? d
            : { ...d, form: { ...d.form, pinned: [...d.form.pinned, hit] } },
        );
    });
  }, [pinTs]);

  const update = (patch: Partial<ReportDraft>) => setDraft((d) => ({ ...d, ...patch }));
  const updateForm = (patch: Partial<ReportForm>) => setDraft((d) => ({ ...d, form: { ...d.form, ...patch } }));
  const discard = () => {
    clearDraft(consoleKey);
    setDraft(defaultDraft(appVersion, detectPlatform(isTauriEnv())));
    setAttached(noImages);
    setResult(null);
    setError(null);
  };

  const save = (built: BuiltReport, since: number) =>
    saveReport(built, { redact: true, since, imagePaths: attached.shots.map((x) => x.path) });
  const create = async () => {
    setBusy(true);
    setError(null);
    const since = rangeStart(draft, Date.now());
    try {
      const built = await buildReport({
        form: draft.form,
        since,
        until: Date.now(),
        cats: new Set(EVENT_CATS),
        sources: new Set(SOURCE_IDS),
        consoles: draft.form.console ? [draft.form.console] : [],
        images: attached.files.map(({ name, base64 }) => ({ name, base64 })),
        platform: isTauriEnv() ? "desktop" : "browser",
      });
      const saved = await save(built, since);
      if (saved) {
        setResult({ built, saved, since });
        clearDraft(consoleKey);
        top.current?.scrollIntoView({ behavior: "smooth", block: "start" });
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const missing = whatIsMissing(draft);
  const blocked =
    missing === "description"
      ? tr("br_blocked_description", undefined, "Describe what went wrong to create the report.")
      : missing === "doing_other"
        ? tr("br_blocked_other", undefined, "Say what you were doing to create the report.")
        : null;

  return (
    // The app's route container scrolls; the action bar sticks to its bottom edge.
    <div className="flex min-h-full flex-col">
      <div className="app-page flex-1">
        <div ref={top} className="mx-auto grid w-full max-w-3xl scroll-mt-4 gap-5">
          <PageHeader
            icon={Bug}
            title={tr("bug_report_page_title", undefined, "Bug report")}
            description={tr(
              "br_page_desc",
              undefined,
              "Tell us what went wrong. The report brings the app's, the engine's and the PS5 helper's own record of what happened, so you don't have to remember it.",
            )}
          />
          {/* Where the report goes, up front: both places are read. */}
          <div
            className="surface-panel flex flex-col gap-3 p-5 sm:flex-row sm:items-center"
            data-testid="br-where"
          >
            <div className="min-w-0 flex-1">
              <div className="text-sm font-medium">{tr("br_where_title", undefined, "Where reports go")}</div>
              <p className="mt-0.5 text-xs leading-relaxed text-[var(--color-muted)]">
                {tr(
                  "br_where_body",
                  undefined,
                  "Fill this in and create the report. You can then post it as a GitHub issue or in the Discord #bugs-report channel. Both are read.",
                )}
              </p>
            </div>
            <div className="flex flex-wrap gap-2 sm:shrink-0">
              <Button size="sm" variant="secondary" leftIcon={<ExternalLink size={14} />} onClick={() => void openExternalUrl(GITHUB_ISSUES_URL)}>
                {tr("br_where_github", undefined, "GitHub issues")}
              </Button>
              <Button size="sm" variant="secondary" leftIcon={<MessageSquare size={14} />} onClick={() => void openExternalUrl(DISCORD_REPORT_URL)}>
                {tr("br_where_discord", undefined, "Discord #bugs-report")}
              </Button>
            </div>
          </div>
          {result && (
            <Card title={tr("br_send_title", undefined, "Send it")} icon={Send} accent>
              <SendPanel
                form={draft.form}
                updateForm={updateForm}
                built={result.built}
                saved={result.saved}
                onDownload={() =>
                  void save(result.built, result.since).catch((e) =>
                    setError(e instanceof Error ? e.message : String(e)),
                  )
                }
              />
            </Card>
          )}
          <Card title={tr("br_step_what", undefined, "What happened")} icon={MessageSquareWarning}>
            <WhatSection form={draft.form} updateForm={updateForm} />
          </Card>
          <Card title={tr("br_screenshots", undefined, "Screenshots")} icon={ImageIcon}>
            <ScreenshotsSection attached={attached} setAttached={setAttached} />
          </Card>
          <Card title={tr("br_section_setup", undefined, "Your setup")} icon={Monitor}>
            <SetupSection form={draft.form} updateForm={updateForm} />
          </Card>
          <Card title={tr("br_section_logs", undefined, "Logs")} icon={ScrollText}>
            <LogsSection draft={draft} update={update} />
          </Card>
          {error && (
            <ErrorCard
              title={tr("br_build_failed", undefined, "Couldn't build the report")}
              detail={humanizePs5Error(error)}
              onDismiss={() => setError(null)}
              action={
                <Button size="sm" onClick={() => void create()} disabled={busy || missing !== null}>
                  {tr("retry", undefined, "Retry")}
                </Button>
              }
            />
          )}
        </div>
      </div>

      {/* Always in reach: what still blocks the report, and the button that creates it. */}
      <div className="sticky bottom-3 z-10 px-4 pb-1 md:px-8 max-md:bottom-[calc(var(--safe-bottom)+5rem)]">
        <div className="glass-float mx-auto flex w-full max-w-3xl flex-wrap items-center gap-x-3 gap-y-1.5 rounded-[var(--radius-panel)] px-4 py-2.5">
          <Button variant="ghost" size="sm" onClick={discard}>
            {tr("br_discard", undefined, "Start over")}
          </Button>
          {blocked && (
            // Its own line on a phone, beside the button on a wider screen.
            <span
              className="order-first basis-full text-xs text-[var(--color-muted)] sm:order-none sm:min-w-0 sm:flex-1 sm:basis-auto sm:text-right"
              data-testid="br-blocked"
            >
              {blocked}
            </span>
          )}
          <Button
            className="ml-auto shrink-0"
            variant="primary"
            leftIcon={<FileArchive size={14} />}
            loading={busy}
            disabled={busy || missing !== null}
            onClick={() => void create()}
          >
            {result ? tr("br_create_again", undefined, "Create again") : tr("br_create", undefined, "Create report")}
          </Button>
        </div>
      </div>
    </div>
  );
}
