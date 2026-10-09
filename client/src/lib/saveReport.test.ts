import { describe, expect, it, vi } from "vitest";

const download = vi.fn(async () => {});
vi.mock("./browserBugBundle", () => ({ downloadBugBundle: (...a: unknown[]) => download(...(a as [])) }));
vi.mock("./tauriEnv", () => ({ isTauriEnv: () => false }));

import { saveReport } from "./saveReport";

describe("saving a report in the browser", () => {
  it("redacts every file once, numbering addresses the same everywhere", async () => {
    await saveReport(
      {
        entries: [
          { path: "timeline.txt", text: "lost 192.168.86.100, then 192.168.86.99" },
          { path: "console/stderr.log", text: "ready 192.168.86.99 pairing_key=abc" },
          { path: "screenshots/a.png", base64: "iVBORw==" },
        ],
        missing: [],
        filename: "r.zip",
        problemLines: [],
        desktop: { engine_log: false, app_logs: false, crash_reports: false },
      },
      { redact: true, since: 0, imagePaths: [] },
    );
    const [entries, name] = download.mock.calls[0] as unknown as [{ path: string; text?: string }[], string];
    expect(name).toBe("r.zip");
    expect(entries[0].text).toBe("lost <ip-1>, then <ip-2>");
    expect(entries[1].text).toBe("ready <ip-2> pairing_key=<removed>");
    expect(JSON.stringify(entries)).not.toMatch(/192\.168/);
  });
});
