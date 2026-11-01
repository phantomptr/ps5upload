import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/remotePlay", async () => {
  const { create } = await import("zustand");
  return { useRemotePlayStore: create(() => ({ stream: null, target: null })) };
});

import { RpBlockerNoticeView, RpBlockerPhaseView, blockerCanCause } from "./RpBlockers";

// No DOM here (vitest runs in node): static markup, asserting on what is shown.
const html = (node: React.ReactNode) => renderToStaticMarkup(<MemoryRouter>{node}</MemoryRouter>);
const base = { heldIds: null, autoPause: false, onAtStart: [], paused: [], error: null, running: false };

describe("Play's Remote Play blocker notices", () => {
  it("asks before a stream when Hush is on", () => {
    const out = html(<RpBlockerNoticeView {...base} heldIds={["hush"]} onAtStart={["hush"]} />);
    expect(out).toContain("Hush is on. It blocks Remote Play&#x27;s picture.");
    expect(out).toContain("Pause Hush while I play");
    expect(out).toContain("Start anyway");
    expect(out).toContain("Always pause Hush for Remote Play");
    expect(html(<RpBlockerNoticeView {...base} />)).toBe("");
  });

  it("shows the pause as a phase over the picture", () => {
    expect(html(<RpBlockerPhaseView ids={["hush"]} />)).toContain("Pausing Hush (about 20 s)…");
  });

  it("names Hush as the cause of a stream that failed while it was on", () => {
    const busy = html(<RpBlockerNoticeView {...base} onAtStart={["hush"]} error={{ code: "busy", detail: "" }} />);
    expect(busy).toContain("Hush was on when this stream started");
    expect(busy).toContain("Restart the console");
    // Also when Play paused it: a session it already blocked can stay stuck.
    expect(
      html(<RpBlockerNoticeView {...base} onAtStart={["hush"]} paused={["hush"]} error={{ code: "busy", detail: "" }} />),
    ).toContain("Restart the console");
    // Not under an error with a cause of its own, nor when Hush was off.
    expect(html(<RpBlockerNoticeView {...base} onAtStart={["hush"]} error={{ code: "keys", detail: "" }} />)).toBe("");
    expect(html(<RpBlockerNoticeView {...base} error={{ code: "busy", detail: "" }} />)).toBe("");
  });

  it("warns during a stream started anyway, not one it paused Hush for", () => {
    expect(html(<RpBlockerNoticeView {...base} onAtStart={["hush"]} running />)).toContain("This stream may have no picture");
    expect(html(<RpBlockerNoticeView {...base} onAtStart={["hush"]} paused={["hush"]} running />)).toBe("");
  });

  it("blames a blocker only for the errors it can cause", () => {
    expect(["busy", "lost", "other"].every(blockerCanCause)).toBe(true);
    expect(["keys", "passcode_cancelled", "passcode_timeout", "asleep", "unreachable", "busy_other", "no_decoder", "socket"].some(blockerCanCause)).toBe(false);
  });
});
