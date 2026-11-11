import type { ReactElement } from "react";
import { renderToStaticMarkup as ssr } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { CollectionLocation } from "../../api/collection";
import {
  isShareLink,
  receiveAccept,
  shareCreate,
  shareDecide,
  ShareError,
  type ReceiveView,
  type ShareView,
} from "../../api/share";
import { ReceiveFlow } from "./ReceivePanel";
import { historyLine } from "./ShareHistory";
import { ShareMade, SharePick, type ShareTarget } from "./SharePanel";
import { appSources, createBody, mediaOf, percent } from "./shareModel";

const render = (el: ReactElement) => ssr(<MemoryRouter>{el}</MemoryRouter>);
const tr = ((_k: string, vars?: Record<string, string | number> | string, fallback?: string) => {
  const s = typeof vars === "string" ? vars : (fallback ?? _k);
  return typeof vars === "object" ? s.replace(/\{(\w+)\}/g, (_, k: string) => String(vars[k])) : s;
}) as Parameters<typeof historyLine>[0];

afterEach(() => vi.unstubAllGlobals());

function copy(type: string, kind?: string): CollectionLocation {
  return {
    root: "/g",
    container: "",
    name: `App.${type}`,
    type,
    path: `App.${type}`,
    absolute_path: `/g/App.${type}`,
    size_bytes: 10,
    added_ts: 0,
    pkg: kind ? { kind } : undefined,
  };
}

const target: ShareTarget = {
  titleId: "PPSA01234",
  title: "Big Game",
  host: null,
  consolePath: "/data/homebrew/PPSA01234",
  copies: [],
};

describe("what can be shared", () => {
  it("offers the console's folder and the Collection's base copies, never an update, DLC or archive", () => {
    const s = appSources("/data/homebrew/PSUP00001", [
      copy("folder"),
      copy("pkg", "patch"),
      copy("pkg", "dlc"),
      copy("zip"),
      copy("mount.ffpkg"),
    ]);
    expect(s.map((x) => (x.kind === "console" ? "console" : x.format))).toEqual(["console", "folder", "mount.ffpkg"]);
    // An image on the console is not its folder.
    expect(appSources("/mnt/ext0/game.ffpkg", [])).toEqual([]);
  });

  it("builds the engine's body from the choices", () => {
    const b = createBody({
      from: " Sam ",
      title: "My App",
      titleId: "PSUP00001",
      console: "192.168.1.5",
      app: { kind: "console", path: "/data/homebrew/PSUP00001" },
      saves: ["/user/home/1/savedata_prospero/PSUP00001"],
      media: ["/user/av_contents/photo/PSUP00001/a.jxr"],
      cheats: [
        { name: "Inf HP", on: true },
        { name: "Speed", on: false },
      ],
      controls: { game: null },
      note: "  have fun  ",
      expiresInS: 3600,
      oneUse: true,
    });
    expect(b.from).toBe("Sam");
    expect(b.items.map((i) => i.kind)).toEqual(["app", "save", "media", "cheats", "controls", "note"]);
    expect(b.items[0]).toEqual({ kind: "app", console: "192.168.1.5", path: "/data/homebrew/PSUP00001" });
    expect(b.items[3].data).toMatchObject({ on: ["Inf HP"] });
    expect(b.items[5].text).toBe("have fun");
    expect(b).toMatchObject({ expires_in_s: 3600, one_use: true });
  });

  it("finds a game's screenshots by its folder", () => {
    const m = mediaOf(
      [{ path: "/user/av_contents/photo/PSUP00001/a.jxr" }, { path: "/user/av_contents/photo/OTHER00001/b.jxr" }, { path: "/user/av_contents/video/My App/c.webm" }],
      "psup00001",
      "My App",
    );
    expect(m).toHaveLength(2);
    expect(percent(5, 10)).toBe(50);
    expect(percent(5, 0)).toBe(0);
  });

  it("knows a Share link", () => {
    expect(isShareLink("https://ps5upload.phantomptr.com/share/#room=AbCdEfGhIjKlMnOpQrStUv&fp=AA&hub=h")).toBe(true);
    expect(isShareLink("http://192.168.1.10:19113/share#room=AbCdEfGhIjKlMnOpQrStUv")).toBe(true);
    expect(isShareLink("https://ps5upload.phantomptr.com/join/#room=AbCdEfGhIjKlMnOpQrStUv")).toBe(false);
  });
});

describe("the Share panel", () => {
  /** The opening tag of the element with this test id. */
  const tag = (html: string, id: string) => new RegExp(`<[^>]*data-testid="${id}"[^>]*>`).exec(html)?.[0] ?? "";
  /** The app row's own checkbox button. */
  const appBox = (html: string) =>
    /<button[^>]*role="checkbox"[^>]*>/.exec(html.slice(html.indexOf('data-testid="share-app"')))?.[0] ?? "";

  it("a refused app is a quiet row: the short reason, the engine's words in the tip, no check", () => {
    const msg = "This is a retail game (PPSA01234). PS5Upload only shares homebrew.";
    const html = render(<SharePick target={target} onMade={() => {}} verdict={msg} />);
    expect(tag(html, "share-app")).toContain('data-state="unavailable"');
    expect(html).toContain("Retail games can&#x27;t be shared");
    expect(tag(html, "share-app-why")).toContain(`aria-label="${msg}"`);
    expect(html).toContain('role="tooltip"');
    expect(appBox(html)).toContain(' disabled=""');
    expect(appBox(html)).toContain('aria-checked="false"');
    // No orange warning text, and no line about stream settings.
    expect(html).not.toContain("color-warn");
    expect(html).not.toContain("aren&#x27;t kept per game");
  });

  it("can't-confirm reads as its own short line", () => {
    const html = render(
      <SharePick target={target} onMade={() => {}} verdict="PS5Upload can't confirm this is homebrew, so it can't share it." />,
    );
    expect(html).toContain("Can&#x27;t confirm it&#x27;s homebrew");
  });

  it("a homebrew app is picked and Share is ready, with the settings as segments", () => {
    const html = render(<SharePick target={{ ...target, titleId: "PSUP00001" }} onMade={() => {}} verdict="" />);
    expect(tag(html, "share-app")).toContain('data-state="on"');
    expect(appBox(html)).toContain('aria-checked="true"');
    expect(html).toContain('data-testid="share-go"');
    expect(tag(html, "share-go")).not.toContain(' disabled=""');
    // Lifetime and who: radio groups, 24 hours and one friend chosen.
    expect(html).toMatch(/role="radio" aria-checked="true"[^>]*>24 hours/);
    expect(html).toMatch(/role="radio" aria-checked="true"[^>]*>One friend/);
    expect(html).toContain("The link stops working after you allow the first person.");
    // The note is one growing line with its placeholder.
    expect(tag(html, "share-note")).toContain('placeholder="A note for your friend (optional)"');
    expect(tag(html, "share-note")).toContain('rows="1"');
    // Your name is shown with an edit button.
    expect(html).toContain('data-testid="share-name-edit"');
  });

  it("only what exists is offered: no console, no saves, screenshots, cheats or controls", () => {
    const html = render(<SharePick target={{ ...target, titleId: "PSUP00001" }} onMade={() => {}} verdict="" />);
    for (const id of ["share-save", "share-media", "share-cheats", "share-controls"]) expect(html).not.toContain(`data-testid="${id}"`);
  });

  it("while the engine checks the app, Share waits", () => {
    const html = render(<SharePick target={target} onMade={() => {}} verdict={null} />);
    expect(tag(html, "share-app")).toContain('data-state="checking"');
    expect(tag(html, "share-go")).toContain(' disabled=""');
  });

  const made: ShareView = {
    id: "s1",
    state: "open",
    from: "Sam",
    title: "My App",
    title_id: "PSUP00001",
    one_use: true,
    expires_in_s: 86_000,
    url: "https://ps5upload.phantomptr.com/share/#room=AbCdEfGhIjKlMnOpQrStUv&fp=AA&hub=h",
    lan_url: null,
    works: "anywhere",
    items: [],
    requests: [
      { request: 1, name: "Alex", wants: ["i1", "i2"], wants_names: ["My App", "Note"], state: "asking", bytes_done: 0, bytes_total: 0, why: null },
      { request: 2, name: "Bo", wants: ["i1"], wants_names: ["My App"], state: "sending", bytes_done: 50, bytes_total: 100, why: null },
    ],
  };

  it("shows the link, the QR code, the time left and who asks, with Allow and Decline", () => {
    const html = render(<ShareMade share={made} onChange={() => {}} onDone={() => {}} />);
    expect(html).toContain('data-testid="share-qr"');
    expect(html).toContain(made.url!.replace(/&/g, "&amp;"));
    expect(html).toContain("Expires in 23 h 53 min");
    expect(html).toContain("Alex asks for");
    expect(html).toContain("My App, Note");
    expect(html.match(/data-testid="share-allow"/g)).toHaveLength(1);
    expect(html.match(/data-testid="share-decline"/g)).toHaveLength(1);
    expect(html).toContain('data-testid="share-cancel"');
    expect(html).toContain('data-testid="share-done"');
    expect(html).toContain("Sending: 50 B of 100 B");
    expect(html).toContain('role="progressbar"');
  });

  it("no one yet: it says it waits", () => {
    const html = render(<ShareMade share={{ ...made, requests: [] }} onChange={() => {}} />);
    expect(html).toContain("Waiting for your friend to open the link");
  });

  it("the native share sheet is offered where the device has one", () => {
    vi.stubGlobal("navigator", { ...navigator, share: () => Promise.resolve() });
    const html = render(<ShareMade share={made} onChange={() => {}} />);
    expect(html).toContain('data-testid="share-native"');
  });

  it("a used or cancelled link shows no link", () => {
    const html = render(<ShareMade share={{ ...made, state: "cancelled", requests: [] }} onChange={() => {}} />);
    expect(html).not.toContain("share-qr");
    expect(html).toContain('data-testid="share-state"');
    expect(html).not.toContain('data-testid="share-cancel"');
  });

  it("Allow and Decline post the friend's request", async () => {
    const calls: [string, unknown][] = [];
    vi.stubGlobal("fetch", async (url: string, init: RequestInit) => {
      calls.push([url, JSON.parse(String(init.body))]);
      return new Response(JSON.stringify(made), { status: 200 });
    });
    await shareDecide("s1", 1, true);
    await shareDecide("s1", 2, false);
    expect(calls[0][0]).toMatch(/\/api\/share\/s1\/decide$/);
    expect(calls.map((c) => c[1])).toEqual([
      { request: 1, allow: true },
      { request: 2, allow: false },
    ]);
  });

  it("a retail refusal from the engine comes back with its Title ID", async () => {
    vi.stubGlobal(
      "fetch",
      async () =>
        new Response(
          JSON.stringify({ error: "This is a retail game (PPSA01234). PS5Upload only shares homebrew.", code: "retail", title_id: "PPSA01234" }),
          { status: 403 },
        ),
    );
    const e = await shareCreate({ items: [{ kind: "app", path: "/g/x" }] }).catch((x: unknown) => x);
    expect(e).toBeInstanceOf(ShareError);
    expect((e as ShareError).code).toBe("retail");
    expect((e as ShareError).titleId).toBe("PPSA01234");
  });

  it("the history in words", () => {
    const at = Date.UTC(2026, 10, 9, 12);
    expect(historyLine(tr, { at_ms: at, dir: "sent", name: "Alex", items: [], state: "done" })).toMatch(/^Shared with Alex, /);
    expect(historyLine(tr, { at_ms: at, dir: "received", name: "Sam", items: [], state: "declined" })).toMatch(/^Received from Sam, .*\(declined\)$/);
  });
});

describe("Receive", () => {
  const offer: ReceiveView = {
    id: "r1",
    state: "offered",
    from: "Sam",
    title: "My App",
    title_id: "PSUP00001",
    items: [
      { id: "i1", kind: "app", name: "My App", title_id: "PSUP00001", format: "folder", size: 2_000_000, files: 3, refused: null },
      { id: "i2", kind: "note", name: "Note", title_id: null, format: null, size: 8, files: 1, refused: null },
      {
        id: "i3",
        kind: "app",
        name: "Big Game",
        title_id: "PPSA01234",
        format: "folder",
        size: 9,
        files: 1,
        refused: "This is a retail game (PPSA01234). PS5Upload only shares homebrew.",
      },
    ],
    accepted: [],
    expires_in_s: 3600,
    bytes_done: 0,
    bytes_total: 0,
    error: null,
    path: null,
    dest: null,
    received: [],
  };

  it("shows who shared what, with retail items quiet and not chosen", () => {
    const html = render(<ReceiveFlow view={offer} host="" onDone={() => {}} onView={() => {}} />);
    expect(html).toContain("Sam shared My App");
    const rows = html.match(/<div[^>]*data-testid="receive-item"[^>]*>/g) ?? [];
    expect(rows).toHaveLength(3);
    expect(rows[0]).toContain('data-state="on"');
    expect(rows[1]).toContain('data-state="on"');
    expect(rows[2]).toContain('data-state="unavailable"');
    expect(html).toContain('data-testid="receive-item-reason"');
    expect(html).toContain("Retail games can&#x27;t be shared");
    expect(html).toContain("1.91 MiB in all");
    expect(html).toContain('data-testid="receive-accept"');
    // No PS5 connected: installing isn't offered.
    expect(html).not.toContain("Install to my PS5");
  });

  it("starts with what is offered ticked even though it first showed Connecting", () => {
    const props = { host: "", onDone: () => {}, onView: () => {} };
    const connecting = ReceiveFlow({ ...props, view: { ...offer, state: "connecting", items: [] } });
    const offered = ReceiveFlow({ ...props, view: offer });
    expect(render(connecting)).toContain('data-testid="receive-connecting"');
    // The offer is another component than the connecting card, so React mounts it when the items
    // arrive and its selection starts from them. (One component for both kept the empty
    // selection it made while connecting: nothing was ticked, and Accept sent nothing.)
    expect(offered.type).not.toBe(connecting.type);
    expect(render(offered).match(/data-testid="receive-item"[^>]*data-state="on"/g) ?? []).toHaveLength(2);
  });

  it("with a PS5 connected, the app can be installed when it arrives", () => {
    const html = render(<ReceiveFlow view={offer} host="192.168.1.5" onDone={() => {}} onView={() => {}} />);
    expect(html).toMatch(/role="radio" aria-checked="true"[^>]*>Keep in Collection/);
    expect(html).toContain("Install to my PS5");
  });

  it("Accept sends only the items left ticked", async () => {
    let body: unknown = null;
    vi.stubGlobal("fetch", async (_u: string, init: RequestInit) => {
      body = JSON.parse(String(init.body));
      return new Response("{}", { status: 200 });
    });
    await receiveAccept("r1", ["i1"]);
    expect(body).toEqual({ items: ["i1"] });
  });

  it("waits for the sender, then shows progress and Cancel", () => {
    const asking = render(<ReceiveFlow view={{ ...offer, state: "asking" }} host="" onDone={() => {}} onView={() => {}} />);
    expect(asking).toContain("Waiting for Sam to allow it");
    const going = render(
      <ReceiveFlow view={{ ...offer, state: "receiving", bytes_done: 1, bytes_total: 2, path: "lan" }} host="" onDone={() => {}} onView={() => {}} />,
    );
    expect(going).toContain('data-testid="receive-cancel"');
    expect(going).toContain("Over your home network.");
    expect(going).toContain("50%");
  });

  it("when done, each item has its action", () => {
    const done: ReceiveView = {
      ...offer,
      state: "done",
      accepted: ["i1", "i2"],
      dest: "/c/Received/My App from Sam",
      received: [
        { item: "i1", kind: "app", path: "/c/Received/My App from Sam/My App" },
        { item: "i2", kind: "note", path: "/c/Received/My App from Sam/Note" },
      ],
    };
    const html = render(<ReceiveFlow view={done} host="192.168.1.5" onDone={() => {}} onView={() => {}} />);
    expect(html).toContain("Received from Sam");
    expect(html).toContain("Saved in /c/Received/My App from Sam");
    expect(html).toContain('data-testid="received-app"');
    expect(html).toContain('data-testid="received-note"');
    expect(html).toContain("It&#x27;s in your Collection now.");
  });

  it("an ended receive says why", () => {
    const html = render(
      <ReceiveFlow view={{ ...offer, state: "refused", error: "used" }} host="" onDone={() => {}} onView={() => {}} />,
    );
    expect(html).toContain("This link was already used.");
  });
});
