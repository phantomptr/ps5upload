/**
 * What a finished bug report turns into (spec §3.2–3.3): report.md for the zip, a pre-filled
 * GitHub issue link, and a short text for Discord. Written in English: they are read by the
 * maintainers, whatever language the app is in.
 */
import type { EventRecord } from "./eventRecord";
import { createRedactor } from "./redaction";
import { formatLine } from "./reportTimeline";

export type Doing =
  | "connecting"
  | "uploading"
  | "installing"
  | "browsing"
  | "launching"
  | "payloads"
  | "other";

export interface ReportForm {
  console: string | null;
  doing: Doing;
  doingOther: string;
  whatHappened: string;
  pinned: EventRecord[];
  steps: string;
  frequency: "once" | "sometimes" | "every_time" | "";
  started: "after_app_update" | "after_fw_update" | "always" | "unknown" | "";
  appVersion: string;
  /** One of the issue form's platform options, verbatim. */
  platform: string;
  firmware: string;
  model: string;
  payloads: string;
  githubIssueUrl: string;
}

export const GITHUB_NEW_ISSUE_URL = "https://github.com/phantomptr/ps5upload/issues/new";
export const GITHUB_URL_MAX = 7500;
export const DISCORD_MAX = 1900;
const TRUNCATED = "(truncated — see zip)";

const DOING_LABEL: Record<Exclude<Doing, "other">, string> = {
  connecting: "Connecting",
  uploading: "Uploading",
  installing: "Installing",
  browsing: "Browsing files",
  launching: "Launching a game",
  payloads: "Payloads",
};
const FREQUENCY_LABEL = { once: "Once", sometimes: "Sometimes", every_time: "Every time", "": "" } as const;
const STARTED_LABEL = {
  after_app_update: "After updating the app",
  after_fw_update: "After a firmware update",
  always: "Always",
  unknown: "Don't know",
  "": "",
} as const;

/** At most `max` characters (code points, never half a surrogate pair), ending in `marker` when cut. */
export function truncateChars(s: string, max: number, marker: string): string {
  const chars = [...s];
  if (chars.length <= max) return s;
  const keep = Math.max(0, max - [...marker].length);
  return chars.slice(0, keep).join("") + marker;
}

function doingLabel(f: ReportForm): string {
  return f.doing === "other" ? f.doingOther.trim() || "Other" : DOING_LABEL[f.doing];
}

export function reportTitle(f: ReportForm): string {
  const first = f.whatHappened.trim().split("\n")[0] ?? "";
  return truncateChars(`[${doingLabel(f)}] ${first}`, 100, "…");
}

function firmwareLine(f: ReportForm): string {
  return [f.firmware, f.model ? `(${f.model})` : ""].filter(Boolean).join(" ");
}

function pinnedLines(f: ReportForm): string[] {
  return f.pinned.map((e) => formatLine(e));
}

function reproText(f: ReportForm): string {
  return [
    f.steps.trim(),
    f.frequency ? `How often: ${FREQUENCY_LABEL[f.frequency]}` : "",
    f.started ? `Started: ${STARTED_LABEL[f.started]}` : "",
  ]
    .filter(Boolean)
    .join("\n\n");
}

/** The full report, nothing cut: report.md in the zip. */
export function reportMarkdown(f: ReportForm, problemLines: string[]): string {
  const pinned = pinnedLines(f);
  return [
    `# ${reportTitle(f)}`,
    "## What happened",
    f.whatHappened.trim(),
    pinned.length ? `## Events this is about\n\n\`\`\`\n${pinned.join("\n")}\n\`\`\`` : "",
    "## Details",
    reproText(f) || "(none given)",
    "## Environment",
    [
      `- App: ${f.appVersion}`,
      `- Platform: ${f.platform}`,
      `- Console: ${f.console ?? "not console-related"}`,
      `- Firmware: ${firmwareLine(f) || "unknown"}`,
      `- Other payloads: ${f.payloads.trim() || "unknown"}`,
    ].join("\n"),
    problemLines.length ? `## Recent warnings and errors\n\n\`\`\`\n${problemLines.join("\n")}\n\`\`\`` : "",
  ]
    .filter(Boolean)
    .join("\n\n");
}

interface IssueFields {
  what: string;
  logs: string;
  repro: string;
  payloads: string;
}

function buildIssueUrl(f: ReportForm, x: IssueFields): string {
  const p = new URLSearchParams({
    template: "bug_report.yml",
    title: reportTitle(f),
    "what-happened": x.what,
    platform: f.platform,
    version: f.appVersion,
    firmware: firmwareLine(f),
    payloads: x.payloads,
    repro: x.repro,
    frequency: f.frequency ? FREQUENCY_LABEL[f.frequency] : "",
    logs: x.logs,
  });
  return `${GITHUB_NEW_ISSUE_URL}?${p.toString()}`;
}

/** Shortens one field to 80% (code points) with the truncation note, until the URL fits. */
function shrinkField(f: ReportForm, x: IssueFields, key: "what" | "repro" | "payloads"): string {
  let url = buildIssueUrl(f, x);
  let chars = [...x[key]].length;
  while (url.length > GITHUB_URL_MAX && chars > 0) {
    chars = Math.floor(chars * 0.8);
    x[key] = truncateChars(x[key], chars, `\n${TRUNCATED}`);
    url = buildIssueUrl(f, x);
  }
  return url;
}

/**
 * The pre-filled "new issue" link, at most GITHUB_URL_MAX characters: cut first the log excerpt,
 * then the description, then the steps, then the payloads. The zip always has everything.
 */
export function githubIssueUrl(f: ReportForm, problemLines: string[]): string {
  const pinned = pinnedLines(f);
  const x: IssueFields = {
    what: [f.whatHappened.trim(), pinned.length ? `Events this is about:\n${pinned.join("\n")}` : ""]
      .filter(Boolean)
      .join("\n\n"),
    logs: "",
    repro: reproText(f),
    payloads: f.payloads.trim(),
  };
  let lines = problemLines.slice(-30);
  const logsOf = (ls: string[], cut: boolean) =>
    [ls.join("\n"), cut ? TRUNCATED : "", "Full report: the attached zip."].filter(Boolean).join("\n");
  x.logs = logsOf(lines, false);
  let url = buildIssueUrl(f, x);
  while (url.length > GITHUB_URL_MAX && lines.length > 0) {
    lines = lines.slice(Math.ceil(lines.length / 4));
    x.logs = logsOf(lines, true);
    url = buildIssueUrl(f, x);
  }
  for (const key of ["what", "repro", "payloads"] as const) {
    if (url.length <= GITHUB_URL_MAX) break;
    url = shrinkField(f, x, key);
  }
  return url;
}

/** The text copied for Discord: at most DISCORD_MAX characters, the description cut first. */
export function discordText(f: ReportForm): string {
  const head = [
    `**${reportTitle(f)}**`,
    `${f.appVersion} · ${f.platform} · FW ${firmwareLine(f) || "?"}`,
    f.payloads.trim() ? `Payloads: ${f.payloads.trim()}` : "",
  ].filter(Boolean);
  const tail = [
    ...(f.pinned.length ? ["```", ...pinnedLines(f).slice(0, 5).map((l) => truncateChars(l, 160, "…")), "```"] : []),
    f.githubIssueUrl.trim() ? `Issue: ${f.githubIssueUrl.trim()}` : "",
    "Full report zip attached.",
  ].filter(Boolean);
  const fixed = [...head, ...tail].join("\n").length + 2;
  const what = truncateChars(f.whatHappened.trim(), Math.max(0, DISCORD_MAX - fixed), "…");
  return truncateChars([...head, what, ...tail].join("\n"), DISCORD_MAX, "…");
}

/**
 * The GitHub link and the Discord text, redacted like the zip: they leave the machine too (a
 * GitHub issue is public). One redactor for both, so <ip-1> is the same console in each.
 */
export function publicOutputs(
  f: ReportForm,
  problemLines: string[],
  redact: boolean,
): { githubUrl: string; discordText: string } {
  const red = createRedactor({ redact });
  const text = (s: string) => red.text(s);
  const safe: ReportForm = {
    ...f,
    console: f.console === null ? null : text(f.console),
    whatHappened: text(f.whatHappened),
    doingOther: text(f.doingOther),
    steps: text(f.steps),
    payloads: text(f.payloads),
    pinned: f.pinned.map((e) => ({ ...e, console: e.console && text(e.console), msg: text(e.msg) })),
  };
  return {
    githubUrl: githubIssueUrl(safe, problemLines.map(text)),
    discordText: discordText(safe),
  };
}
