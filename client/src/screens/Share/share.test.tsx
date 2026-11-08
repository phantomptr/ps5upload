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
  it("shows the engine's retail refusal and leaves the app unticked", () => {
    const msg = "This is a retail game (PPSA01234). PS5Upload only shares homebrew.";
    const html = render(<SharePick target={target} onMade={() => {}} verdict={msg} />);
    expect(html).toContain('data-testid="share-app-refused"');
    expect(html).toContain(msg);
    // The app's checkbox is off and can't be turned on.
    const box = /<input[^>]*type="checkbox"[^>]*>/.exec(html.slice(html.indexOf('data-testid="share-app"')))?.[0] ?? "";
    expect(box).toContain("disabled");
    expect(box).not.toMatch(/checked=""/);
    // Stream settings: said, not offered.
    expect(html).toContain('data-testid="share-stream-none"');
  });

  it("a homebrew app is ticked and Share is ready", () => {
    const html = render(<SharePick target={{ ...target, titleId: "PSUP00001" }} onMade={() => {}} verdict="" />);
    expect(html).not.toContain("share-app-refused");
    expect(html).toContain('data-testid="share-go"');
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

  it("shows the link, the QR code and who asks, with Allow and Decline", () => {
    const html = render(<ShareMade share={made} onChange={() => {}} />);
    expect(html).toContain('data-testid="share-qr"');
    expect(html).toContain(made.url!.replace(/&/g, "&amp;"));
    expect(html).toContain("Alex wants: My App, Note");
    expect(html.match(/data-testid="share-allow"/g)).toHaveLength(1);
    expect(html.match(/data-testid="share-decline"/g)).toHaveLength(1);
    expect(html).toContain('data-testid="share-cancel"');
    expect(html).toContain("Sending:");
  });

  it("a used or cancelled link shows no link", () => {
    const html = render(<ShareMade share={{ ...made, state: "cancelled", requests: [] }} onChange={() => {}} />);
    expect(html).not.toContain("share-qr");
    expect(html).toContain('data-testid="share-state"');
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

  it("shows who shared what, with retail items marked and not chosen", () => {
    const html = render(<ReceiveFlow view={offer} host="" onDone={() => {}} onView={() => {}} />);
    expect(html).toContain("Sam shared My App");
    expect(html).toContain("My App · ");
    expect(html).toContain('data-testid="receive-item-refused"');
    const boxes = html.match(/<input[^>]*type="checkbox"[^>]*>/g) ?? [];
    expect(boxes).toHaveLength(3);
    expect(boxes[0]).toMatch(/checked=""/);
    expect(boxes[1]).toMatch(/checked=""/);
    expect(boxes[2]).toContain("disabled");
    expect(boxes[2]).not.toMatch(/checked=""/);
    expect(html).toContain('data-testid="receive-accept"');
    // No PS5 connected: installing isn't offered.
    expect(html).not.toContain("Install it to my PS5");
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
