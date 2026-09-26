import { describe, expect, it } from "vitest";

import { captureCrashReport } from "./crashReporter";
import { reportProblem } from "./reportProblem";

/**
 * The reporter's one hard contract: it must NEVER throw. It runs in the exact
 * conditions it exists to capture (errors, crashes), so a throwing reporter
 * would turn one failure into two. These run without a Tauri runtime, so the
 * backend `invoke`/dialog/shell calls reject — the helpers must swallow that.
 */
describe("crash reporter robustness", () => {
  it("captureCrashReport resolves (never throws) with no Tauri runtime", async () => {
    await expect(
      captureCrashReport("test-trigger", { force: true }),
    ).resolves.toBeUndefined();
  });

  it("reportProblem returns a graceful result instead of throwing", async () => {
    const r = await reportProblem("test");
    expect(r).toBeTruthy();
    expect(typeof r.ok).toBe("boolean");
    // No Tauri runtime → the save dialog / zip call fails, so the flow
    // reports ok=false rather than throwing.
    expect(r.ok).toBe(false);
  });
});
