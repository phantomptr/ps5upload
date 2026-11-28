import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

const env = vi.hoisted(() => ({ tauri: false }));
vi.mock("../../lib/tauriEnv", () => ({ isTauriEnv: () => env.tauri }));
vi.mock("../../state/lang", () => ({
  useTr: () => (key: string, a?: unknown, b?: string) => {
    const vars = typeof a === "object" && a ? (a as Record<string, string | number>) : {};
    let s = (typeof a === "string" ? a : b) ?? key;
    for (const [k, v] of Object.entries(vars)) s = s.replace(`{${k}}`, String(v));
    return s;
  },
}));

import type { ReportForm } from "../../lib/reportOutputs";
import SendPanel, { fileNameOf } from "./SendPanel";

const form: ReportForm = {
  console: null,
  doing: "connecting",
  doingOther: "",
  whatHappened: "x",
  pinned: [],
  steps: "",
  frequency: "",
  started: "",
  appVersion: "12.6.44",
  platform: "macOS (Apple Silicon)",
  firmware: "",
  model: "",
  payloads: "",
  githubIssueUrl: "",
};
const built = {
  entries: [],
  missing: [],
  filename: "ps5upload-report.zip",
  problemLines: [],
  desktop: { engine_log: true, app_logs: true, crash_reports: true },
};

const render = (dest: string) =>
  renderToStaticMarkup(
    <SendPanel form={form} updateForm={() => {}} built={built} saved={{ dest, bytes: 1 }} onDownload={() => {}} />,
  );

afterEach(() => {
  env.tauri = false;
});

describe("SendPanel: the zip is dragged in by hand", () => {
  it("asks first that the zip will be dragged into the issue, and names it", () => {
    env.tauri = true;
    const html = render("/Users/me/Desktop/ps5upload-report-2026-11-27.zip");
    expect(html).toContain("I&#x27;ll drag the zip into the issue before I submit it");
    expect(html).toMatch(/<button[^>]*disabled=""[^>]*data-testid="br-post-github"|<button[^>]*data-testid="br-post-github"[^>]*disabled=""/);
    expect(html).toContain("The app can&#x27;t attach the zip for you");
    expect(html).toContain("drag ps5upload-report-2026-11-27.zip into the &quot;Bug report zip&quot; box");
    expect(html).toContain("/Users/me/Desktop/ps5upload-report-2026-11-27.zip");
    expect(html).toContain('data-testid="br-show-zip"');
    expect(html).toContain("Show in folder");
  });

  it("in a browser names the download and offers no folder to show", () => {
    const html = render("ps5upload-report.zip");
    expect(html).toContain("ps5upload-report.zip, in your browser&#x27;s downloads");
    expect(html).not.toContain("br-show-zip");
  });

  it("reads the file name from either separator", () => {
    expect(fileNameOf("C:\\Users\\me\\report.zip")).toBe("report.zip");
    expect(fileNameOf("/home/me/report.zip")).toBe("report.zip");
    expect(fileNameOf("report.zip")).toBe("report.zip");
  });
});
