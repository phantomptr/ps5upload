import { afterEach, describe, expect, it } from "vitest";

import { FOCUSABLE_SELECTOR } from "./focusTrap";
import { handleTvKey, installTvNav, isTvDevice, keepsArrow, tvNavActive } from "./tvNav";

/*
 * Vitest runs in node (no jsdom in the client): a small fake DOM with what tvNav touches —
 * closest / querySelectorAll for attribute selectors, focus, getBoundingClientRect,
 * activeElement and documentElement.dataset.
 */
class FakeEl {
  parent: FakeEl | null = null;
  children: FakeEl[] = [];
  disabled = false;
  isContentEditable = false;
  type = "";
  constructor(
    readonly doc: FakeDoc,
    readonly tagName: string,
    readonly attrs: Record<string, string> = {},
    readonly box = { left: 0, top: 0, width: 0, height: 0 },
  ) {}
  add(...kids: FakeEl[]): this {
    for (const k of kids) {
      k.parent = this;
      this.children.push(k);
    }
    return this;
  }
  getAttribute(n: string): string | null {
    return n in this.attrs ? this.attrs[n] : null;
  }
  matches(sel: string): boolean {
    return sel.split(",").some((s) => simple(this, s.trim()));
  }
  closest(sel: string): FakeEl | null {
    if (this.matches(sel)) return this;
    return this.parent ? this.parent.closest(sel) : null;
  }
  querySelectorAll(sel: string): FakeEl[] {
    const all = this.children.flatMap((c) => [c, ...c.descendants()]);
    return all.filter((e) => e.matches(sel));
  }
  descendants(): FakeEl[] {
    return this.children.flatMap((c) => [c, ...c.descendants()]);
  }
  getBoundingClientRect(): DOMRect {
    const { left, top, width, height } = this.box;
    return { left, top, width, height, right: left + width, bottom: top + height, x: left, y: top } as DOMRect;
  }
  focus(): void {
    this.doc.activeElement = this;
  }
  scrollIntoView(): void {}
}

/** One simple selector: a tag, `[attr]` or `[attr="v"]`, each with optional `:not(...)`. */
function simple(el: FakeEl, sel: string): boolean {
  const nots = [...sel.matchAll(/:not\(([^)]*)\)/g)].map((m) => m[1]);
  if (nots.some((n) => simple(el, n))) return false;
  const base = sel.replace(/:not\([^)]*\)/g, "");
  if (base === "[disabled]") return el.disabled;
  const attr = /^\[([\w-]+)(?:=['"]([^'"]*)['"])?\]$/.exec(base);
  if (attr) return attr[1] in el.attrs && (attr[2] === undefined || el.attrs[attr[1]] === attr[2]);
  const tagged = /^([a-z]+)(\[[^\]]+\])?$/.exec(base);
  if (!tagged || el.tagName !== tagged[1].toUpperCase()) return false;
  if (tagged[2]) return simple(el, tagged[2]);
  return true;
}

class FakeDoc {
  activeElement: FakeEl | null = null;
  documentElement = { dataset: {} as Record<string, string> };
  body: FakeEl;
  constructor() {
    this.body = new FakeEl(this, "BODY");
  }
  querySelectorAll(sel: string): FakeEl[] {
    return this.body.querySelectorAll(sel);
  }
}

class FakeKey {
  defaultPrevented = false;
  altKey = false;
  ctrlKey = false;
  metaKey = false;
  shiftKey = false;
  constructor(readonly key: string) {}
  preventDefault(): void {
    this.defaultPrevented = true;
  }
}

function button(doc: FakeDoc, name: string, left: number, top: number, attrs: Record<string, string> = {}) {
  return new FakeEl(doc, "BUTTON", { name, ...attrs }, { left, top, width: 80, height: 30 });
}

/** A page: two rows of content buttons and a bottom tab bar. */
function page() {
  const doc = new FakeDoc();
  const a = button(doc, "a", 0, 0);
  const b = button(doc, "b", 100, 0);
  const c = button(doc, "c", 0, 50);
  const home = button(doc, "home", 0, 500);
  const files = button(doc, "files", 100, 500, { "aria-current": "page" });
  const tabbar = new FakeEl(doc, "NAV").add(home, files);
  doc.body.add(a, b, c, tabbar);
  return { doc, a, b, c, home, files };
}

function press(doc: FakeDoc, key: string, already = false) {
  const e = new FakeKey(key);
  if (already) e.preventDefault();
  const moved = handleTvKey(e as unknown as KeyboardEvent, doc as unknown as Document);
  return { moved, prevented: e.defaultPrevented, focused: (doc.activeElement?.attrs.name as string) ?? null };
}

describe("tvNav", () => {
  it("the selector it relies on is the focus trap's", () => {
    expect(FOCUSABLE_SELECTOR).toContain("button:not([disabled])");
  });

  it("the first press lands on the current page's tab", () => {
    const { doc } = page();
    expect(press(doc, "ArrowDown")).toEqual({ moved: true, prevented: true, focused: "files" });
  });

  it("arrows move to the nearest control that way, down to the tab bar", () => {
    const { doc, a } = page();
    a.focus();
    expect(press(doc, "ArrowRight").focused).toBe("b");
    expect(press(doc, "ArrowLeft").focused).toBe("a");
    expect(press(doc, "ArrowDown").focused).toBe("c");
    expect(press(doc, "ArrowDown").focused).toBe("home");
    expect(press(doc, "ArrowRight").focused).toBe("files");
    expect(press(doc, "ArrowUp").focused).toBe("b");
  });

  it("at the edge the key is left alone, so the page scrolls", () => {
    const { doc, a } = page();
    a.focus();
    expect(press(doc, "ArrowUp")).toEqual({ moved: false, prevented: false, focused: "a" });
  });

  it("a control that used the key keeps it", () => {
    const { doc, a } = page();
    a.focus();
    expect(press(doc, "ArrowDown", true)).toEqual({ moved: false, prevented: true, focused: "a" });
  });

  it("other keys and modified arrows do nothing", () => {
    const { doc, a } = page();
    a.focus();
    expect(press(doc, "Enter").moved).toBe(false);
    const e = new FakeKey("ArrowDown");
    e.shiftKey = true;
    expect(handleTvKey(e as unknown as KeyboardEvent, doc as unknown as Document)).toBe(false);
  });

  it("disabled, hidden and script-only controls are skipped", () => {
    const { doc, a, c } = page();
    c.disabled = true;
    const hidden = button(doc, "hidden", 0, 30, { "aria-hidden": "true" });
    const scriptOnly = button(doc, "scriptOnly", 0, 40, { tabindex: "-1" });
    const empty = new FakeEl(doc, "BUTTON", { name: "empty" }, { left: 0, top: 45, width: 0, height: 0 });
    doc.body.add(hidden, scriptOnly, empty);
    a.focus();
    expect(press(doc, "ArrowDown").focused).toBe("home");
  });

  it("inside a dialog, focus stays in the dialog", () => {
    const { doc } = page();
    const ok = button(doc, "ok", 0, 200);
    const cancel = button(doc, "cancel", 100, 200);
    doc.body.add(new FakeEl(doc, "DIV", { role: "dialog", "aria-modal": "true" }).add(ok, cancel));
    ok.focus();
    expect(press(doc, "ArrowRight").focused).toBe("cancel");
    // The tab bar lies below, outside the dialog.
    expect(press(doc, "ArrowDown")).toEqual({ moved: false, prevented: false, focused: "cancel" });
  });

  it("text fields keep Left and Right for the caret, a text area keeps all four", () => {
    const doc = new FakeDoc();
    const input = new FakeEl(doc, "INPUT");
    input.type = "text";
    expect(keepsArrow(input as unknown as Element, "left")).toBe(true);
    expect(keepsArrow(input as unknown as Element, "down")).toBe(false);
    const range = new FakeEl(doc, "INPUT");
    range.type = "range";
    expect(keepsArrow(range as unknown as Element, "right")).toBe(true);
    expect(keepsArrow(range as unknown as Element, "up")).toBe(false);
    const checkbox = new FakeEl(doc, "INPUT");
    checkbox.type = "checkbox";
    expect(keepsArrow(checkbox as unknown as Element, "left")).toBe(false);
    expect(keepsArrow(new FakeEl(doc, "TEXTAREA") as unknown as Element, "down")).toBe(true);
    expect(keepsArrow(new FakeEl(doc, "BUTTON") as unknown as Element, "down")).toBe(false);
    expect(keepsArrow(null, "down")).toBe(false);
  });

  it("the Remote Play picture keeps the arrows for the game", () => {
    const { doc } = page();
    const stage = new FakeEl(doc, "DIV", { tabindex: "0", "data-own-arrows": "" }, { left: 0, top: 100, width: 300, height: 200 });
    doc.body.add(stage);
    stage.focus();
    expect(keepsArrow(stage as unknown as Element, "down")).toBe(true);
    expect(press(doc, "ArrowDown").moved).toBe(false);
  });
});

describe("installTvNav", () => {
  const listeners: Array<(e: KeyboardEvent) => void> = [];
  function fakeWindow(tv: boolean | "throws") {
    const doc = page().doc;
    return {
      doc,
      w: {
        document: doc,
        PS5UploadTv: {
          isTv: () => {
            if (tv === "throws") throw new Error("bridge gone");
            return tv;
          },
        },
        addEventListener: (_t: string, fn: (e: KeyboardEvent) => void) => void listeners.push(fn),
      } as unknown as Window,
    };
  }
  afterEach(() => {
    listeners.length = 0;
  });

  it("is off away from a TV, and when the bridge fails", () => {
    expect(isTvDevice(undefined)).toBe(false);
    expect(isTvDevice({} as Window)).toBe(false);
    expect(isTvDevice(fakeWindow("throws").w)).toBe(false);
    const { w, doc } = fakeWindow(false);
    expect(installTvNav(w)).toBe(false);
    expect(listeners).toHaveLength(0);
    expect(tvNavActive(doc as unknown as Document)).toBe(false);
  });

  it("on a TV it marks the page and moves focus with the arrows", () => {
    const { w, doc } = fakeWindow(true);
    expect(installTvNav(w)).toBe(true);
    expect(tvNavActive(doc as unknown as Document)).toBe(true);
    expect(listeners).toHaveLength(1);
    const e = new FakeKey("ArrowDown");
    listeners[0](e as unknown as KeyboardEvent);
    expect(doc.activeElement?.attrs.name).toBe("files");
    // Once only.
    expect(installTvNav(w)).toBe(true);
    expect(listeners).toHaveLength(1);
  });
});
