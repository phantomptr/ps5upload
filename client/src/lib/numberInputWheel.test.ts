import { describe, expect, it } from "vitest";
import { installNumberInputWheelGuard } from "./numberInputWheel";

/** A document with one wheel listener slot and a focus. */
function fakeDoc() {
  const doc = {
    activeElement: null as unknown,
    listener: null as ((e: Event) => void) | null,
    options: null as unknown,
    addEventListener(type: string, fn: (e: Event) => void, o: unknown) {
      if (type === "wheel") {
        doc.listener = fn;
        doc.options = o;
      }
    },
    removeEventListener(type: string, fn: (e: Event) => void) {
      if (type === "wheel" && doc.listener === fn) doc.listener = null;
    },
  };
  return doc;
}
function box(doc: ReturnType<typeof fakeDoc>, type: string, focused: boolean) {
  const el = {
    tagName: "INPUT",
    type,
    blur() {
      if (doc.activeElement === el) doc.activeElement = null;
    },
  };
  if (focused) doc.activeElement = el;
  return el;
}
const wheel = (doc: ReturnType<typeof fakeDoc>, target: unknown) => doc.listener?.({ target } as unknown as Event);

describe("installNumberInputWheelGuard", () => {
  it("listens early (capture) and passively", () => {
    const doc = fakeDoc();
    installNumberInputWheelGuard(doc as unknown as Document);
    expect(doc.options).toEqual({ capture: true, passive: true });
  });
  it("takes the focus off a focused number box the wheel turns over", () => {
    const doc = fakeDoc();
    installNumberInputWheelGuard(doc as unknown as Document);
    const n = box(doc, "number", true);
    wheel(doc, n);
    expect(doc.activeElement).toBeNull();
  });
  it("leaves text boxes and unfocused number boxes alone", () => {
    const doc = fakeDoc();
    installNumberInputWheelGuard(doc as unknown as Document);
    const t = box(doc, "text", true);
    wheel(doc, t);
    expect(doc.activeElement).toBe(t);
    wheel(doc, box(doc, "number", false));
    expect(doc.activeElement).toBe(t);
    wheel(doc, null);
    expect(doc.activeElement).toBe(t);
  });
  it("can be removed", () => {
    const doc = fakeDoc();
    installNumberInputWheelGuard(doc as unknown as Document)();
    expect(doc.listener).toBeNull();
  });
});
