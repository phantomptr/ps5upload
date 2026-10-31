import { isValidElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import {
  BadgeCheck,
  Download,
  Hash,
  PackagePlus,
  Pencil,
  Trash2,
  Upload,
} from "lucide-react";

vi.mock("../../state/lang", () => ({
  useTr: () =>
    (key: string, vars?: Record<string, string | number>, fallback?: string) => {
      let s = fallback ?? key;
      for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
      return s;
    },
}));

import {
  RowActionButtonList,
  buildRowActions,
  RowActionsTrigger,
  RowMenuList,
  menuPlacement,
  rowActionMenuItems,
  type RowAction,
  type RowActionsContext,
} from "./RowActions";
import { SELECTION_ACTION, SelectionBar } from "./SelectionBar";
import { RecentPathsPanel } from "./RecentPathsDropdown";

// No DOM here (vitest in node): render to markup for what a user sees, and walk
// the element tree to press the buttons.

function fileActions(): RowAction[] {
  return [
    { id: "download", icon: Download, label: "Download", title: "Save a copy", run: vi.fn() },
    { id: "rename", icon: Pencil, label: "Rename", run: vi.fn() },
    { id: "replace", icon: Upload, label: "Replace file", run: vi.fn(), disabled: true },
    { id: "crc32", icon: Hash, label: "CRC32 checksum", run: vi.fn() },
    { id: "verify", icon: BadgeCheck, label: "Verify (BLAKE3)", run: vi.fn() },
    { id: "install", icon: PackagePlus, label: "Install", run: vi.fn(), tone: "accent" },
    { id: "delete", icon: Trash2, label: "Delete", run: vi.fn(), tone: "bad" },
  ];
}

/** Every <button> element in a rendered tree (host elements only). */
function buttons(node: ReactNode): ReactElement<{ onClick: () => void }>[] {
  const out: ReactElement<{ onClick: () => void }>[] = [];
  const walk = (n: ReactNode) => {
    if (Array.isArray(n)) return n.forEach(walk);
    if (!isValidElement(n)) return;
    const el = n as ReactElement<{ children?: ReactNode; onClick: () => void }>;
    if (el.type === "button") out.push(el);
    walk(el.props.children);
  };
  walk(node);
  return out;
}

const labelsIn = (html: string, attr: RegExp) =>
  [...html.matchAll(attr)].map((m) => m[1]);

describe("Files row: the phone ⋮ menu and the desktop buttons", () => {
  it("the menu lists every desktop action, in the same order, Delete last in danger colour", () => {
    const actions = fileActions();
    const desktop = renderToStaticMarkup(<RowActionButtonList actions={actions} />);
    const menu = renderToStaticMarkup(
      <div>
        <RowMenuList items={rowActionMenuItems(actions)} onPick={() => {}} />
      </div>,
    );
    const desktopLabels = labelsIn(desktop, /aria-label="([^"]+)"/g);
    const menuLabels = labelsIn(menu, /<\/svg>([^<]+)<\/button>/g);
    expect(desktopLabels).toEqual(actions.map((a) => a.label));
    expect(menuLabels).toEqual(desktopLabels);
    expect((menu.match(/role="menuitem"/g) ?? []).length).toBe(actions.length);
    // Delete is last and red; nothing else is.
    const items = menu.split("<button").slice(1);
    expect(items[items.length - 1]).toContain("text-[var(--color-bad)]");
    expect(items.slice(0, -1).join("")).not.toContain("text-[var(--color-bad)]");
    // A disabled action is disabled in both.
    expect(items[2]).toContain("disabled");
  });

  it("each menu item runs the same handler as its desktop button", () => {
    const actions = fileActions();
    const desktop = buttons(RowActionButtonList({ actions }));
    const menu = buttons(
      RowMenuList({ items: rowActionMenuItems(actions), onPick: (it) => it.run() }),
    );
    expect(desktop).toHaveLength(actions.length);
    expect(menu).toHaveLength(actions.length);
    actions.forEach((a, i) => {
      desktop[i].props.onClick();
      expect(a.run).toHaveBeenCalledTimes(1);
      menu[i].props.onClick();
      expect(a.run).toHaveBeenCalledTimes(2);
    });
  });

  it("desktop buttons hide below sm and the ⋮ shows only there", () => {
    const desktop = renderToStaticMarkup(<RowActionButtonList actions={fileActions()} />);
    expect(desktop).toMatch(/class="[^"]*\bhidden\b[^"]*\bsm:flex\b/);
    const trigger = renderToStaticMarkup(
      <RowActionsTrigger label="Actions for a.pkg" open={false} onToggle={() => {}} />,
    );
    expect(trigger).toContain("sm:hidden");
    expect(trigger).toContain('aria-haspopup="menu"');
    expect(trigger).toContain('aria-expanded="false"');
    expect(trigger).toContain('aria-label="Actions for a.pkg"');
  });

  it("a busy action shows a spinner on desktop and is not offered in the menu", () => {
    const actions: RowAction[] = [
      { id: "install", icon: PackagePlus, label: "Install", run: vi.fn(), busy: true, tone: "accent" },
    ];
    expect(renderToStaticMarkup(<RowActionButtonList actions={actions} />)).toContain("animate-spin");
    expect(rowActionMenuItems(actions)[0].disabled).toBe(true);
  });
});

describe("buildRowActions: what a Files row offers", () => {
  const tr = ((key: string, _vars?: unknown, fallback?: string) =>
    fallback ?? key) as unknown as RowActionsContext["tr"];
  const ctx = (): RowActionsContext => ({
    tr,
    desktopApp: true,
    hasHost: true,
    downloadActive: false,
    downloadingName: null,
    entryBusy: false,
    pkgInstalling: false,
    installingPkgName: null,
    viewable: (name) => name.endsWith(".pkg"),
    download: vi.fn(),
    rename: vi.fn(),
    replace: vi.fn(),
    view: vi.fn(),
    preview: vi.fn(),
    crc32: vi.fn(),
    verify: vi.fn(),
    install: vi.fn(),
    remove: vi.fn(),
  });

  it("a package file: every action, in the desktop order, each running its handler", () => {
    const c = ctx();
    const e = { name: "game.pkg", kind: "file", size: 101502976 };
    const actions = buildRowActions(e, c);
    expect(actions.map((a) => a.id)).toEqual([
      "download",
      "download-zip",
      "rename",
      "replace",
      "view",
      "crc32",
      "verify",
      "install",
      "delete",
    ]);
    const menu = buttons(
      RowMenuList({ items: rowActionMenuItems(actions), onPick: (it) => it.run() }),
    );
    menu.forEach((b) => b.props.onClick());
    expect(c.download).toHaveBeenNthCalledWith(1, e);
    expect(c.download).toHaveBeenNthCalledWith(2, e, true);
    for (const h of [c.rename, c.replace, c.view, c.crc32, c.verify, c.install, c.remove])
      expect(h).toHaveBeenCalledWith(e);
    expect(c.preview).not.toHaveBeenCalled();
  });

  it("a folder in the browser build: rename and delete only", () => {
    const c = { ...ctx(), desktopApp: false };
    const actions = buildRowActions({ name: "PS4", kind: "dir", size: 0 }, c);
    expect(actions.map((a) => a.id)).toEqual(["rename", "delete"]);
  });

  it("a file in the browser build offers no checksum actions (desktop-only commands)", () => {
    const c = { ...ctx(), desktopApp: false };
    const ids = buildRowActions({ name: "game.pkg", kind: "file", size: 9e8 }, c).map((a) => a.id);
    expect(ids).not.toContain("crc32");
    expect(ids).not.toContain("verify");
  });

  it("properties keeps Rename first in the browser menu and Delete last", () => {
    const properties = vi.fn();
    const c = { ...ctx(), desktopApp: false, properties };
    const e = { name: "PS4", kind: "dir", size: 0 };
    const actions = buildRowActions(e, c);
    expect(actions.map((a) => a.id)).toEqual(["rename", "properties", "delete"]);
    actions[1].run();
    expect(properties).toHaveBeenCalledWith(e);
  });

  it("a small file gets Preview; a running install shows on its own row only", () => {
    const c = { ...ctx(), installingPkgName: "a.pkg", pkgInstalling: true };
    const small = buildRowActions({ name: "readme.txt", kind: "file", size: 512 }, c);
    expect(small.map((a) => a.id)).toContain("preview");
    const a = buildRowActions({ name: "a.pkg", kind: "file", size: 9e8 }, c);
    const b = buildRowActions({ name: "b.pkg", kind: "file", size: 9e8 }, c);
    expect(a.find((x) => x.id === "install")?.busy).toBe(true);
    expect(b.find((x) => x.id === "install")?.busy).toBe(false);
    expect(b.find((x) => x.id === "install")?.disabled).toBe(true);
  });

  it("a .elf/.bin offers Load payload after Install's place, held while a package installs", () => {
    const loadPayload = vi.fn();
    const c = { ...ctx(), loadPayload, loadingPayloadName: "k.elf" };
    const e = { name: "k.elf", kind: "file", size: 300_000 };
    const ids = buildRowActions(e, c).map((a) => a.id);
    expect(ids.indexOf("load-payload")).toBe(ids.length - 2);
    const act = buildRowActions(e, c).find((a) => a.id === "load-payload");
    expect(act?.busy).toBe(true);
    act?.run();
    expect(loadPayload).toHaveBeenCalledWith(e);
    expect(buildRowActions({ name: "x.BIN", kind: "file", size: 9 }, c).some((a) => a.id === "load-payload")).toBe(true);
    expect(buildRowActions({ name: "x.txt", kind: "file", size: 9 }, c).some((a) => a.id === "load-payload")).toBe(false);
    // no handler (older screen wiring): not offered
    expect(buildRowActions(e, ctx()).some((a) => a.id === "load-payload")).toBe(false);
    const held = buildRowActions(e, { ...c, pkgInstalling: true }).find((a) => a.id === "load-payload");
    expect(held?.disabled).toBe(true);
  });
});

describe("menuPlacement", () => {
  const vp = { width: 360, height: 800 };
  const menu = { width: 200, height: 300 };

  it("opens under the ⋮, its end edge on the button's", () => {
    const anchor = { left: 310, right: 340, top: 100, bottom: 130 };
    expect(menuPlacement(anchor, menu, vp)).toEqual({ left: 140, top: 130 });
  });

  it("flips above the ⋮ near the bottom of the screen", () => {
    const anchor = { left: 310, right: 340, top: 700, bottom: 730 };
    expect(menuPlacement(anchor, menu, vp)).toEqual({ left: 140, top: 400 });
  });

  it("stays inside the viewport when neither side has room", () => {
    const anchor = { left: 0, right: 30, top: 200, bottom: 230 };
    const tall = { width: 200, height: 700 };
    const pos = menuPlacement(anchor, tall, vp);
    expect(pos.left).toBe(4);
    expect(pos.top).toBe(96);
    expect(pos.top + tall.height).toBeLessThanOrEqual(vp.height);
  });

  // A phone with one file in the folder: no room under the row, and flipping
  // up put Download under the status bar, where Android takes the tap.
  it("never puts the menu under the status bar or the navigation bar", () => {
    const phone = { width: 412, height: 915, insetTop: 32, insetBottom: 48 };
    const anchor = { left: 360, right: 400, top: 440, bottom: 490 };
    const pos = menuPlacement(anchor, { width: 200, height: 420 }, phone);
    expect(pos.top).toBeGreaterThanOrEqual(phone.insetTop + 4);
    expect(pos.top + 420).toBeLessThanOrEqual(phone.height - phone.insetBottom - 4);
  });
});

describe("Files row: snapshot and what takes space (menu only)", () => {
  const tr = ((key: string, _vars?: unknown, fallback?: string) =>
    fallback ?? key) as unknown as RowActionsContext["tr"];
  const ctx = (): RowActionsContext => ({
    tr,
    desktopApp: false,
    hasHost: true,
    downloadActive: false,
    downloadingName: null,
    entryBusy: false,
    pkgInstalling: false,
    installingPkgName: null,
    viewable: () => false,
    download: vi.fn(),
    rename: vi.fn(),
    replace: vi.fn(),
    view: vi.fn(),
    preview: vi.fn(),
    crc32: vi.fn(),
    verify: vi.fn(),
    install: vi.fn(),
    remove: vi.fn(),
    snapshot: vi.fn(),
    space: vi.fn(),
  });

  it("a folder offers both in the ⋮ menu, before Delete, and draws neither as a row button", () => {
    const c = ctx();
    const dir = { name: "homebrew", kind: "dir", size: 0 };
    const actions = buildRowActions(dir, c);
    expect(actions.map((a) => a.id).slice(-3)).toEqual(["space", "snapshot", "delete"]);
    actions.find((a) => a.id === "snapshot")?.run();
    expect(c.snapshot).toHaveBeenCalledWith(dir);
    actions.find((a) => a.id === "space")?.run();
    expect(c.space).toHaveBeenCalledWith(dir);
    const html = renderToStaticMarkup(<RowActionButtonList actions={actions} />);
    expect(html).not.toContain("Snapshot");
    expect(html).not.toContain("What takes space?");
  });

  it("a file can be snapshotted but has no space map", () => {
    const ids = buildRowActions({ name: "a.txt", kind: "file", size: 3 }, ctx()).map((a) => a.id);
    expect(ids).toContain("snapshot");
    expect(ids).not.toContain("space");
  });
});

describe("Files selection bar", () => {
  it("keeps the count on one line and scrolls the actions on a phone", () => {
    const action = "Set permissions (777)";
    const html = renderToStaticMarkup(
      <SelectionBar countLabel="1 selected" clearLabel="Clear selection" onClear={() => {}}>
        <button type="button" className={SELECTION_ACTION}>
          {action}
        </button>
      </SelectionBar>,
    );
    expect(html).toMatch(/<span class="[^"]*shrink-0[^"]*whitespace-nowrap[^"]*">1 selected<\/span>/);
    const strip = html.match(/data-testid="fs-selection-actions" class="([^"]+)"/)?.[1] ?? "";
    expect(strip).toContain("max-sm:overflow-x-auto");
    expect(strip).toContain("min-w-0");
    expect(strip).not.toContain("flex-wrap");
    expect(SELECTION_ACTION).toContain("max-sm:whitespace-nowrap");
    expect(SELECTION_ACTION).toContain("max-sm:shrink-0");
    expect(html).toContain('aria-label="Clear selection"');
  });
});

describe("Files Recent dropdown", () => {
  it("opens on an opaque raised surface above the list", () => {
    const html = renderToStaticMarkup(
      <RecentPathsPanel
        paths={["/mnt", "/data/homebrew"]}
        onPick={() => {}}
        onRemove={() => {}}
        onClear={() => {}}
      />,
    );
    const cls = html.match(/data-testid="fs-recent-list" class="([^"]+)"/)?.[1] ?? "";
    expect(cls).toContain("glass-float");
    expect(cls).toContain("z-40");
    // The translucent page tint is what let the rows below read through it.
    expect(cls).not.toContain("bg-[var(--color-surface)]");
    expect(html).toContain("/data/homebrew");
  });
});
