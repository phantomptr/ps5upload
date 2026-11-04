import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { anchorOf, closeModalFocus, modalFocusTarget, refocusIfLost, restoreFocus } from "./restoreFocus";

/*
 * No DOM in vitest here: a fake with what restoreFocus touches (closest,
 * querySelector(All) for list items and focusables, isConnected, focus).
 */
class El {
  parent: El | null = null;
  children: El[] = [];
  constructor(
    readonly name: string,
    readonly attrs: Record<string, string> = {},
    readonly tagName = "DIV",
  ) {}
  add(...kids: El[]): this {
    for (const k of kids) {
      k.parent = this;
      this.children.push(k);
    }
    return this;
  }
  remove(): void {
    if (!this.parent) return;
    this.parent.children = this.parent.children.filter((c) => c !== this);
    this.parent = null;
  }
  get isConnected(): boolean {
    return topOf(this) === root;
  }
  matches(sel: string): boolean {
    if (sel === "[data-nav-list]") return "data-nav-list" in this.attrs;
    if (sel === "[data-nav-item]") return "data-nav-item" in this.attrs;
    if (sel === ".cw-content") return this.attrs.class === "cw-content";
    if (sel === "[data-nav-modal]") return "data-nav-modal" in this.attrs;
    const act = /^\[data-action="([^"]+)"\]$/.exec(sel);
    if (act) return this.attrs["data-action"] === act[1];
    if (sel === "[data-nav-default]:not([disabled])") return "data-nav-default" in this.attrs && !("disabled" in this.attrs);
    return this.tagName === "BUTTON" && !("disabled" in this.attrs); // the focusable selector
  }
  closest(sel: string): El | null {
    return this.matches(sel) ? this : (this.parent?.closest(sel) ?? null);
  }
  descendants(): El[] {
    return this.children.flatMap((c) => [c, ...c.descendants()]);
  }
  querySelectorAll(sel: string): El[] {
    return this.descendants().filter((e) => e.matches(sel));
  }
  querySelector(sel: string): El | null {
    return this.querySelectorAll(sel)[0] ?? null;
  }
  focus(): void {
    doc.activeElement = this;
  }
}

function topOf(e: El): El {
  return e.parent ? topOf(e.parent) : e;
}

const root = new El("root");
const doc = { activeElement: null as El | null, body: root };

let content: El;
let list: El;
let rows: El[];
let up: El;

const row = (i: number) =>
  new El(`row${i}`, { "data-nav-item": "" }, "LI").add(new El(`open${i}`, {}, "BUTTON"), new El(`more${i}`, {}, "BUTTON"));

beforeEach(() => {
  root.children = [];
  rows = [0, 1, 2].map(row);
  list = new El("list", { "data-nav-list": "" }, "UL").add(...rows);
  up = new El("up", {}, "BUTTON");
  content = new El("content", { class: "cw-content" }).add(up, list);
  root.add(content);
  (globalThis as any).document = doc;
});

afterEach(() => {
  delete (globalThis as any).document;
});

const btnOf = (r: El, i = 0) => r.children[i];

describe("ConfirmDialog focus restore", () => {
  it("goes back to the opener while it is still there", () => {
    const a = anchorOf(btnOf(rows[1], 1) as unknown as HTMLElement);
    expect(restoreFocus(a)).toBe(btnOf(rows[1], 1));
  });

  it("a deleted row: the item now at the same place", () => {
    const a = anchorOf(btnOf(rows[1]) as unknown as HTMLElement);
    rows[1].remove();
    restoreFocus(a);
    expect(doc.activeElement).toBe(btnOf(rows[2]));
  });

  it("the last row deleted: the nearest remaining one", () => {
    const a = anchorOf(btnOf(rows[2]) as unknown as HTMLElement);
    rows[2].remove();
    restoreFocus(a);
    expect(doc.activeElement).toBe(btnOf(rows[1]));
  });

  it("the list emptied: the section's first control, never the rail", () => {
    const a = anchorOf(btnOf(rows[0]) as unknown as HTMLElement);
    rows.forEach((r) => r.remove());
    restoreFocus(a);
    expect(doc.activeElement).toBe(up);
  });

  it("nothing to remember: nothing happens", () => {
    expect(restoreFocus(anchorOf(null))).toBeNull();
  });
});

describe("a modal's first focus", () => {
  const button = (name: string, attrs: Record<string, string> = {}) => new El(name, attrs, "BUTTON");
  const target = (modal: El) => modalFocusTarget(modal as unknown as HTMLElement) as unknown as El | null;

  it("its default control", () => {
    const a = button("a");
    const b = button("b", { "data-nav-default": "" });
    expect(target(new El("modal").add(a, b))).toBe(b);
  });

  it("a disabled default is passed over: the first control that can take focus", () => {
    // A .pkg's sheet while a batch runs: Install comes first and is held.
    const install = button("install", { "data-nav-default": "", disabled: "" });
    const copy = button("copy");
    expect(target(new El("modal").add(install, copy))).toBe(copy);
  });

  it("no default: its first control; nothing to focus: nothing", () => {
    const a = button("a");
    expect(target(new El("modal").add(new El("text"), a))).toBe(a);
    expect(target(new El("modal"))).toBeNull();
    expect(modalFocusTarget(null)).toBeNull();
  });

  it("a control given directly is the target", () => {
    const safe = button("safe");
    expect(target(safe)).toBe(safe);
  });
});

describe("a modal closing", () => {
  it("focus goes back to the opener", () => {
    const a = anchorOf(btnOf(rows[1]) as unknown as HTMLElement);
    doc.activeElement = up;
    expect(closeModalFocus(a)).toBe(btnOf(rows[1]));
    expect(doc.activeElement).toBe(btnOf(rows[1]));
  });

  it("the opener is gone and focus was lost: its stand-in", () => {
    const a = anchorOf(btnOf(rows[1]) as unknown as HTMLElement);
    rows[1].remove();
    doc.activeElement = doc.body;
    closeModalFocus(a);
    expect(doc.activeElement).toBe(btnOf(rows[2]));
  });

  it("a sheet and its confirm close together: the stand-in never takes focus from the sheet's opener", () => {
    // Confirm's opener was the sheet's button, gone with the sheet; the sheet already put focus back.
    const sheet = new El("sheet", { "data-nav-modal": "" });
    const uninstall = new El("uninstall", {}, "BUTTON");
    content.add(sheet.add(uninstall));
    const card = btnOf(rows[2]);
    const confirm = anchorOf(uninstall as unknown as HTMLElement);
    const opener = anchorOf(card as unknown as HTMLElement);
    sheet.remove();
    // Whichever cleanup React runs first, focus ends on the card.
    doc.activeElement = doc.body;
    closeModalFocus(opener);
    closeModalFocus(confirm);
    expect(doc.activeElement).toBe(card);
    doc.activeElement = doc.body;
    closeModalFocus(confirm);
    closeModalFocus(opener);
    expect(doc.activeElement).toBe(card);
  });
});

describe("refocusIfLost (a dialog button that removed itself)", () => {
  let modal: El;
  let close: El;
  beforeEach(() => {
    close = new El("close", {}, "BUTTON");
    modal = new El("modal", { "data-nav-modal": "" });
    root.add(modal);
  });

  it("Calculate became Cancel: focus goes to Cancel, not the dialog's first control", () => {
    const calc = new El("calc", { "data-action": "size-start" }, "BUTTON");
    modal.add(close, calc);
    calc.focus();
    calc.remove();
    const cancel = new El("cancel", { "data-action": "size-cancel" }, "BUTTON");
    modal.add(cancel);
    expect(refocusIfLost(modal as unknown as ParentNode, ['[data-action="size-cancel"]', '[data-action="size-start"]'])).toBe(cancel);
    expect(doc.activeElement).toBe(cancel);
  });

  it("Keep both gone: the next preferred control, else the first one", () => {
    const keep = new El("keep", { "data-action": "unpack-keep" }, "BUTTON");
    const go = new El("go", { "data-action": "unpack-go" }, "BUTTON");
    modal.add(close, keep, go);
    keep.focus();
    keep.remove();
    refocusIfLost(modal as unknown as ParentNode, ['[data-action="unpack-go"]']);
    expect(doc.activeElement).toBe(go);
    go.remove();
    refocusIfLost(modal as unknown as ParentNode, ['[data-action="unpack-go"]']);
    expect(doc.activeElement).toBe(close);
  });

  it("focus still on a live control: left alone", () => {
    const go = new El("go", { "data-action": "unpack-go" }, "BUTTON");
    modal.add(close, go);
    close.focus();
    expect(refocusIfLost(modal as unknown as ParentNode, ['[data-action="unpack-go"]'])).toBeNull();
    expect(doc.activeElement).toBe(close);
  });
});
