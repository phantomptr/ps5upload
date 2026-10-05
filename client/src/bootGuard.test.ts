// @ts-expect-error -- the app tsconfig has no node types; Vitest runs this under Node.
import { readFileSync } from "node:fs";
// @ts-expect-error -- as above.
import vm from "node:vm";
import { describe, expect, it } from "vitest";

const SRC: string = readFileSync(new URL("../public/boot-guard.js", import.meta.url), "utf8");

interface Node_ {
  tagName: string;
  id: string;
  children: Node_[];
  text: string;
  parentNode: Node_ | null;
  firstChild: Node_ | null;
  style: Record<string, string>;
  setAttribute(): void;
  appendChild(c: Node_): Node_;
  removeChild(c: Node_): void;
}

function el(tagName: string): Node_ {
  const n: Node_ = {
    tagName,
    id: "",
    children: [],
    text: "",
    parentNode: null,
    firstChild: null,
    style: {},
    setAttribute() {},
    appendChild(c) {
      c.parentNode = n;
      n.children.push(c);
      n.firstChild = n.children[0] ?? null;
      return c;
    },
    removeChild(c) {
      n.children = n.children.filter((x) => x !== c);
      n.firstChild = n.children[0] ?? null;
    },
  };
  return n;
}

function harness(over: Record<string, unknown> = {}) {
  const body = el("BODY");
  const root = el("DIV");
  root.id = "root";
  const listeners: Record<string, Array<(e: unknown) => void>> = {};
  const timers: Array<() => void> = [];
  const doc = {
    body,
    documentElement: body,
    getElementById: (id: string) => (id === "root" ? root : null),
    createElement: (t: string) => el(t.toUpperCase()),
    createTextNode: (text: string) => Object.assign(el("#text"), { text }),
  };
  const win: Record<string, unknown> = {
    Promise,
    fetch: () => {},
    Map,
    Set,
    navigator: { userAgent: "UA-test" },
    addEventListener: (k: string, f: (e: unknown) => void) => (listeners[k] ||= []).push(f),
    setTimeout: (f: () => void) => timers.push(f),
    ...over,
  };
  vm.runInNewContext(SRC, { window: win, document: doc, Object, Array });
  const notice = () => body.children.find((c) => c.id === "ps5u-boot-notice");
  return { root, listeners, timers, notice, mount: () => root.appendChild(el("DIV")) };
}

describe("boot guard (#352)", () => {
  it("is plain ES5: nothing the oldest WebView could fail to parse", () => {
    const code = SRC.replace(/\/\*[\s\S]*?\*\//g, "");
    expect(code).not.toMatch(/=>|`|\bconst\b|\blet\b|\?\.|\?\?|\bclass\b|\basync\b|\bawait\b|\.\.\./);
    expect(() => new vm.Script(SRC)).not.toThrow();
  });

  it("shows nothing when the app starts", () => {
    const h = harness();
    h.mount();
    h.timers[0]();
    expect(h.notice()).toBeUndefined();
  });

  it("shows the notice when nothing rendered by the timeout", () => {
    const h = harness();
    h.timers[0]();
    expect(h.notice()).toBeDefined();
  });

  it("removes the notice if the app does start late", () => {
    const h = harness();
    h.timers[0]();
    h.mount();
    h.timers[1]();
    expect(h.notice()).toBeUndefined();
  });

  it("shows the notice at once on a SyntaxError", () => {
    const h = harness();
    h.listeners.error[0]({ message: "SyntaxError: Unexpected token '?'", target: {} });
    expect(h.notice()).toBeDefined();
  });

  it("shows the notice when a script fails to load", () => {
    const h = harness();
    h.listeners.error[0]({ target: { tagName: "SCRIPT" } });
    expect(h.notice()).toBeDefined();
  });

  it("ignores an unrelated runtime error", () => {
    const h = harness();
    h.listeners.error[0]({ message: "Network request failed", target: {} });
    expect(h.notice()).toBeUndefined();
  });

  it("shows the notice at once when a needed feature is missing", () => {
    const h = harness({ fetch: undefined });
    expect(h.notice()).toBeDefined();
  });
});
