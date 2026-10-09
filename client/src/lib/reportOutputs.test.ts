import { describe, expect, it } from "vitest";
import {
  DISCORD_MAX,
  GITHUB_URL_MAX,
  discordText,
  githubIssueUrl,
  reportMarkdown,
  reportTitle,
  truncateChars,
  type ReportForm,
} from "./reportOutputs";

const form = (over: Partial<ReportForm> = {}): ReportForm => ({
  console: "PS5 Pro",
  doing: "connecting",
  doingOther: "",
  whatHappened: "It disconnected and won't come back until I reboot",
  pinned: [],
  steps: "",
  frequency: "sometimes",
  started: "unknown",
  appVersion: "6.5.2",
  platform: "macOS (Apple Silicon)",
  firmware: "13.60",
  model: "CFI-7019",
  payloads: "kstuff-lite 1.11, ShadowMountPlus 1.7beta4",
  githubIssueUrl: "",
  ...over,
});

describe("githubIssueUrl", () => {
  it("prefills the issue form's fields", () => {
    const u = new URL(githubIssueUrl(form(), ["18:27 [helper] [fatal] signal 11"]));
    expect(u.origin + u.pathname).toBe("https://github.com/phantomptr/ps5upload/issues/new");
    const p = u.searchParams;
    expect(p.get("template")).toBe("bug_report.yml");
    expect(p.get("title")).toBe("[Connecting] It disconnected and won't come back until I reboot");
    expect(p.get("platform")).toBe("macOS (Apple Silicon)");
    expect(p.get("version")).toBe("6.5.2");
    expect(p.get("firmware")).toBe("13.60 (CFI-7019)");
    expect(p.get("frequency")).toBe("Sometimes");
    expect(p.get("payloads")).toContain("kstuff-lite");
    expect(p.get("logs")).toContain("[fatal] signal 11");
  });

  it("stays under the limit, cutting logs before the description", () => {
    const u = githubIssueUrl(form({ whatHappened: "多".repeat(3000) }), Array(500).fill("x".repeat(80)));
    expect(u.length).toBeLessThanOrEqual(GITHUB_URL_MAX);
    const p = new URL(u).searchParams;
    expect(p.get("logs")).toContain("(truncated — see zip)");
  });

  it("keeps the description whole when cutting the logs is enough", () => {
    const u = githubIssueUrl(form(), Array(500).fill("y".repeat(80)));
    expect(new URL(u).searchParams.get("what-happened")).toContain("It disconnected");
  });
});

describe("discordText", () => {
  it("fits Discord and includes the issue link", () => {
    const t = discordText(
      form({ whatHappened: "y".repeat(5000), githubIssueUrl: "https://github.com/phantomptr/ps5upload/issues/999" }),
    );
    expect(t.length).toBeLessThanOrEqual(DISCORD_MAX);
    expect(t).toContain("issues/999");
    expect(t).toContain("6.5.2");
  });
});

describe("truncateChars", () => {
  it("never splits a surrogate pair", () => {
    const s = truncateChars("😀".repeat(10), 5, "…");
    expect([...s].every((c) => c === "😀" || c === "…")).toBe(true);
    expect([...s].length).toBeLessThanOrEqual(5);
  });
  it("leaves short text alone", () => {
    expect(truncateChars("abc", 5, "…")).toBe("abc");
  });
});

describe("reportTitle", () => {
  it("uses the Other text and caps the length", () => {
    const t = reportTitle(form({ doing: "other", doingOther: "Cheats", whatHappened: "a".repeat(300) }));
    expect(t.startsWith("[Cheats] ")).toBe(true);
    expect(t.length).toBeLessThanOrEqual(100);
  });
});

describe("reportMarkdown", () => {
  it("has every answer, untruncated", () => {
    const md = reportMarkdown(form({ whatHappened: "z".repeat(9000), steps: "1. open Hardware" }), ["line"]);
    expect(md).toContain("z".repeat(9000));
    expect(md).toContain("1. open Hardware");
    expect(md).toContain("CFI-7019");
  });
});
