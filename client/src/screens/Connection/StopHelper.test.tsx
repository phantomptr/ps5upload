import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { StopHelperView, stopHelperFlow, type StopHelperDeps } from "./StopHelper";
import type { Translator } from "../../state/lang";
import type { ConfirmOptions } from "../../components/ConfirmDialog";

const LABEL = "Stop helper";
const tr = ((_k: string, a?: unknown, b?: string) => {
  const s = typeof a === "string" ? a : (b ?? _k);
  const vars = (typeof a === "object" && a) || {};
  return s.replace(/\{(\w+)\}/g, (_, k: string) => String((vars as Record<string, unknown>)[k] ?? ""));
}) as Translator;

function deps(over: Partial<StopHelperDeps> = {}) {
  const d = {
    confirm: vi.fn(async () => true),
    busy: vi.fn(() => false),
    stop: vi.fn(async () => ({ state: "stopped" as const })),
    onStopped: vi.fn(),
    ...over,
  };
  return d;
}

describe("Stop helper", () => {
  it("asks first, saying what the helper is needed for", async () => {
    const confirm = vi.fn(async (_o: ConfirmOptions) => true);
    const d = deps({ confirm });
    await stopHelperFlow("192.0.2.99", d, tr);
    expect(confirm).toHaveBeenCalledTimes(1);
    const opts = confirm.mock.calls[0][0];
    expect(opts.title).toBe("Stop the helper?");
    expect(opts.message).toContain("send it again from here");
    expect(opts.confirmLabel).toBe("Stop helper");
    expect(opts.destructive).toBe(true);
  });

  it("does nothing when the user cancels", async () => {
    const d = deps({ confirm: vi.fn(async () => false) });
    expect(await stopHelperFlow("192.0.2.99", d, tr)).toEqual({ kind: "cancelled" });
    expect(d.stop).not.toHaveBeenCalled();
    expect(d.onStopped).not.toHaveBeenCalled();
  });

  it("stops the helper and marks the console", async () => {
    const d = deps();
    expect(await stopHelperFlow(" 192.0.2.99 ", d, tr)).toEqual({ kind: "stopped" });
    expect(d.stop).toHaveBeenCalledWith("192.0.2.99");
    expect(d.onStopped).toHaveBeenCalledWith("192.0.2.99");
  });

  it("treats 'nothing was running' as stopped too", async () => {
    const d = deps({ stop: vi.fn(async () => ({ state: "not_running" as const })) });
    expect((await stopHelperFlow("h", d, tr)).kind).toBe("stopped");
    expect(d.onStopped).toHaveBeenCalled();
  });

  it("refuses while this app is moving something, without asking the engine", async () => {
    const d = deps({ busy: vi.fn(() => true) });
    const out = await stopHelperFlow("h", d, tr);
    expect(out.kind).toBe("refused");
    expect(out.kind === "refused" && out.message).toContain("The helper is in use");
    expect(d.stop).not.toHaveBeenCalled();
    expect(d.onStopped).not.toHaveBeenCalled();
  });

  it("says the same when the engine refuses because a job is running", async () => {
    const d = deps({
      stop: vi.fn(async () => ({ state: "refused" as const, reason: "busy", error: "x" })),
    });
    const out = await stopHelperFlow("h", d, tr);
    expect(out.kind === "refused" && out.message).toContain("The helper is in use");
    expect(d.onStopped).not.toHaveBeenCalled();
  });

  it("passes any other refusal or failure on, unmarked", async () => {
    const refused = deps({
      stop: vi.fn(async () => ({
        state: "refused" as const,
        reason: "refused",
        error: "pair with this console",
      })),
    });
    const a = await stopHelperFlow("h", refused, tr);
    expect(a.kind === "refused" && a.message).toBe("Couldn't stop the helper: pair with this console");
    const thrown = deps({ stop: vi.fn(async () => Promise.reject(new Error("engine down"))) });
    const b = await stopHelperFlow("h", thrown, tr);
    expect(b.kind === "refused" && b.message).toContain("engine down");
    expect(refused.onStopped).not.toHaveBeenCalled();
    expect(thrown.onStopped).not.toHaveBeenCalled();
  });

  it("renders the button and the refusal", () => {
    const html = renderToStaticMarkup(
      <StopHelperView label={LABEL} working={false} message="The helper is in use." onClick={() => {}} />,
    );
    expect(html).toContain("Stop helper");
    expect(html).toContain('role="alert"');
    expect(html).toContain("The helper is in use.");
    const quiet = renderToStaticMarkup(
      <StopHelperView label={LABEL} working={false} message={null} onClick={() => {}} />,
    );
    expect(quiet).not.toContain('role="alert"');
  });
});
