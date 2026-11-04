import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", () => ({
  useTr: () =>
    (key: string, vars?: Record<string, string | number>, fallback?: string) => {
      let s = fallback ?? key;
      for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
      return s;
    },
}));

import { parseConnectionTest } from "../../api/ava1";
import { testLines } from "../../lib/c2cTest";
import { TestResultLines } from "./SendToConsoleDialog";
import { RemoteSendRow } from "../Activity/RemoteSends";

// No DOM here (vitest in node): render to markup and check what a user would see.
describe("Send to another console: Test connection", () => {
  it("shows one line per verdict", () => {
    const lines = testLines(
      parseConnectionTest({
        direct: { verdict: "timeout", target: "127.0.0.1:19120" },
        relay: { from: { verdict: "ok" }, to: { verdict: "ok" } },
      }),
      "Pro",
      "Phat",
    );
    const html = renderToStaticMarkup(<TestResultLines lines={lines} error={null} />);
    expect(html).toContain("Pro got no answer from 127.0.0.1:19120");
    expect(html).toContain("This device reaches both consoles");
    expect((html.match(/<li/g) ?? []).length).toBe(2);
  });

  it("says when the test could not run at all (an older engine)", () => {
    const html = renderToStaticMarkup(
      <TestResultLines lines={[]} error="the connection test failed (404)" />,
    );
    expect(html).toContain("The test did not run: the connection test failed (404)");
    expect(html).toContain('role="alert"');
  });
});

describe("Tasks: a send from another device", () => {
  const base = {
    txId: "0".repeat(32),
    from: "192.0.2.100",
    to: "192.0.2.99",
    src: "/data/homebrew/GAME",
    dest: "/data/homebrew/GAME",
    route: "auto" as const,
    directAddr: null,
    ownerName: "Phone",
    mine: false,
    confirmed: true,
    bytesDone: 1 << 20,
    bytesTotal: 4 << 20,
    updatedMs: 1,
    error: null,
    console: "192.0.2.99",
  };
  const row = (over: object) =>
    renderToStaticMarkup(
      <RemoteSendRow
        send={{ ...base, cls: "running_elsewhere", liveRoute: "direct", canWatch: true, canTakeOver: true, ...over }}
        fromName="Pro"
        toName="Phat"
        onAction={() => {}}
      />,
    );

  it("offers Watch and Take over while it runs direct", () => {
    const html = row({});
    expect(html).toContain("Running on Phone");
    expect(html).toContain("Watch");
    expect(html).toContain("Take over");
    expect(html).toContain("Pro → Phat");
  });

  it("explains a relay (no Watch) and older helpers (no Take over)", () => {
    const relay = row({ liveRoute: "relay", canWatch: false });
    expect(relay).not.toContain(">Watch<");
    expect(relay).toContain("can be taken over but not watched");
    const old = row({ canWatch: false, canTakeOver: false });
    expect(old).toContain("needs the newer helper on both consoles");
    expect(old).not.toContain("Take over<");
  });

  it("offers Resume here for a paused or stopped send", () => {
    expect(row({ cls: "paused", liveRoute: null })).toContain("Resume here");
    const stopped = row({ cls: "interrupted", confirmed: false, liveRoute: null });
    expect(stopped).toContain("Stopped on Phone");
    expect(stopped).toContain("It may still be running");
  });
});
