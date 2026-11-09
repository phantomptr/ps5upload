import type { ReactElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import { installWindowStub } from "../../test-utils/storage";
import { PlayTogetherStartView, togetherStartKind, type PlayTogetherStartViewProps } from "./PlayTogetherStart";

// The page itself, with the stream and the guest side as stand-ins: which page shows, and what
// the stream panel is told.
vi.mock("./StreamPanel", () => ({
  StreamPanel: ({ together }: { together?: boolean }) => <div data-testid="stream-panel" data-together={String(!!together)} />,
}));
vi.mock("./JoinAFriend", () => ({ JoinAFriend: () => <div data-testid="join-a-friend" /> }));

const html = (el: ReactElement, at = "/") => renderToStaticMarkup(<MemoryRouter initialEntries={[at]}>{el}</MemoryRouter>);

const view = (p: Partial<PlayTogetherStartViewProps> = {}) =>
  html(
    <PlayTogetherStartView
      consoleName="Living room"
      canStart
      starting={false}
      startLabel="Start and invite"
      onStart={() => {}}
      playAs={null}
      invites="anywhere"
      {...p}
    />,
  );

afterEach(() => vi.unstubAllGlobals());

describe("Play's pages", () => {
  it("Play my PS5, Play Together, Join a friend, in that order; Play my PS5 first (Receive is on Share)", async () => {
    installWindowStub();
    const { default: PlayScreen } = await import("./index");
    const out = html(<PlayScreen />, "/play");
    const radios = [...out.matchAll(/role="radio" aria-checked="(true|false)"[^>]*>(?:<svg[\s\S]*?<\/svg>)?([^<]+)</g)].map((m) => [m[2], m[1]]);
    expect(radios).toEqual([
      ["Play my PS5", "true"],
      ["Play Together", "false"],
      ["Join a friend", "false"],
    ]);
    expect(out).toContain('data-testid="stream-panel" data-together="false"');
  });

  it("a join link lands on Join a friend", async () => {
    installWindowStub();
    const { default: PlayScreen } = await import("./index");
    const out = html(<PlayScreen />, `/play?join=${encodeURIComponent("http://10.0.0.5:19113/join#i=00")}`);
    expect(out).toMatch(/role="radio" aria-checked="true"[^>]*>(?:<svg[\s\S]*?<\/svg>)?Join a friend</);
    expect(out).toContain('data-testid="join-a-friend"');
    expect(out).not.toContain("stream-panel");
  });

  it("a Share link lands on the Share screen, filled in", async () => {
    installWindowStub();
    const { default: ShareScreen } = await import("../Share");
    const link = "http://10.0.0.5:19113/share#room=AbCdEfGhIjKlMnOpQrStUv";
    const out = html(<ShareScreen />, `/share?receive=${encodeURIComponent(link)}`);
    expect(out).toContain('data-testid="share-screen"');
    expect(out).toContain('data-testid="receive-panel"');
    expect(out).toContain(link.replace(/&/g, "&amp;"));
  });
});

describe("Start and invite", () => {
  it("says what it is, the console, and one button: no network to choose", () => {
    installWindowStub();
    const out = view();
    expect(out).toContain("No app needed");
    expect(out).toMatch(/data-testid="pt-start-console">.*Living room/);
    expect(out).toMatch(/<button[^>]*data-testid="pt-start-invite"[^>]*>[\s\S]*Start and invite<\/span><\/button>/);
    expect(out).not.toMatch(/<button[^>]* disabled=""[^>]*data-testid="pt-start-invite"/);
    // One link that works anywhere: its hint and the support line, no Local or Internet choice.
    expect(out).toContain('data-testid="pt-start-net-internet-hint"');
    expect(out).toContain('data-testid="pt-start-net-internet-support"');
    expect(out).not.toContain("pt-start-net-home");
    expect(out).not.toContain('data-testid="pt-start-net-internet"');
  });

  it("a console not set up: the button is greyed out", () => {
    installWindowStub();
    expect(view({ canStart: false, consoleName: null })).toMatch(/<button[^>]* disabled=""[^>]*data-testid="pt-start-invite"/);
  });

  it("an old saved Internet choice changes nothing", () => {
    installWindowStub({ "ps5upload.pt_network": "internet" });
    expect(view()).not.toContain("pt-start-net-home");
  });

  it("only on the Wi-Fi in the settings: said, with the way there", () => {
    installWindowStub();
    const off = view({ invites: "wifi" });
    expect(off).toContain('data-testid="pt-start-net-off"');
    expect(off).toContain("Invites work only on your Wi-Fi");
    expect(off).toContain("Open settings");
    expect(off).not.toContain("pt-start-net-internet-hint");
  });

  it("an engine with no internet invites, or not read yet: nothing about networks", () => {
    installWindowStub();
    for (const invites of [null, undefined]) {
      const out = view({ invites });
      expect(out).not.toContain("pt-start-net");
    }
  });

  it("starts, switches from another console, or shows the stream started elsewhere", () => {
    const base = { otherStream: false, streaming: false, elsewhere: null, host: "10.0.0.2" };
    expect(togetherStartKind(base)).toBe("start");
    expect(togetherStartKind({ ...base, otherStream: true, streaming: true })).toBe("switch");
    // Started from the API or MCP (the engine has it, this app does not show it).
    expect(togetherStartKind({ ...base, elsewhere: "10.0.0.2" })).toBe("attach");
    // Elsewhere on another console: this one starts as usual.
    expect(togetherStartKind({ ...base, elsewhere: "10.0.0.9" })).toBe("start");
  });
});
