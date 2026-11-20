import { beforeEach, describe, expect, it } from "vitest";

import type { Translator } from "./lang";
import { noteRedeployWhy, redeployWhyText, useRedeployStatusStore } from "./redeployStatus";

const tr = ((_k: string, vars: Record<string, string | number> | undefined, fallback: string) =>
  fallback.replace(/\{(\w+)\}/g, (_, n) => String(vars?.[n] ?? ""))) as unknown as Translator;

beforeEach(() => useRedeployStatusStore.setState({ byHost: {} }));

describe("why the helper is not sent by itself", () => {
  it("is kept per console, by bare host, and cleared with null", () => {
    noteRedeployWhy("192.168.1.20:9120", { kind: "loader-closed" });
    expect(useRedeployStatusStore.getState().byHost["192.168.1.20"]).toEqual({ kind: "loader-closed" });
    noteRedeployWhy("192.168.1.20", null);
    expect(useRedeployStatusStore.getState().byHost).toEqual({});
  });

  it("says exactly why, for each case the user would otherwise have to guess", () => {
    expect(redeployWhyText(tr, { kind: "setting-off" })).toContain("Reconnect automatically is off");
    expect(redeployWhyText(tr, { kind: "loader-closed" })).toContain("port 9021");
    expect(redeployWhyText(tr, { kind: "busy" })).toContain("transfer");
    expect(redeployWhyText(tr, { kind: "alive" })).toContain("still answers on its port");
    expect(redeployWhyText(tr, { kind: "sending" })).toBe("Starting the helper…");
    expect(redeployWhyText(tr, { kind: "answering" })).toContain("can't use it");
    expect(redeployWhyText(tr, { kind: "console-busy" })).toContain("busy");
    expect(redeployWhyText(tr, { kind: "stale-build" })).toContain("another version");
    expect(redeployWhyText(tr, { kind: "limit", code: "send_budget_spent", retryAt: 30 * 60_000 }, 0)).toContain(
      "about 30 min",
    );
    expect(redeployWhyText(tr, { kind: "limit", code: "console_restarted_after_sends", retryAt: null })).toContain(
      "restarted twice",
    );
    expect(redeployWhyText(tr, { kind: "limit", code: "different_console", retryAt: null })).toContain(
      "Another PS5",
    );
    expect(redeployWhyText(tr, { kind: "settling", sendAt: 70_000 }, 10_000)).toContain("in about 60 s");
    expect(redeployWhyText(tr, { kind: "gave-up", retryAt: 15 * 60_000 }, 0)).toBe(
      "Sent 3 times without the helper starting. The next try is in about 15 min, or press Send helper now.",
    );
    expect(redeployWhyText(tr, undefined)).toBeNull();
  });

  it("never counts down below one", () => {
    expect(redeployWhyText(tr, { kind: "settling", sendAt: 0 }, 10_000)).toContain("about 1 s");
    expect(redeployWhyText(tr, { kind: "gave-up", retryAt: 0 }, 10_000)).toContain("about 1 min");
  });
});
