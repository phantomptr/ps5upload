import { describe, expect, it } from "vitest";
import { applyNavInset, navInsetCssPx } from "./androidInsets";

function fakeWindow(bridge?: { navBottomPx(): number }, dpr = 2.625) {
  const props = new Map<string, string>();
  const w = {
    devicePixelRatio: dpr,
    PS5UploadInsets: bridge,
    document: {
      documentElement: {
        style: { setProperty: (k: string, v: string) => void props.set(k, v) },
      },
    },
  } as unknown as Window;
  return { w, props };
}

describe("android navigation-bar inset", () => {
  /* The emulator's gesture bar: 126 physical px at 2.625 dpr = 48 CSS px. */
  it("converts the shell's physical pixels to CSS pixels", () => {
    expect(navInsetCssPx(126, 2.625)).toBe(48);
    expect(navInsetCssPx(0, 2.625)).toBe(0);
    expect(navInsetCssPx(-5, 2)).toBe(0);
    expect(navInsetCssPx(126, 0)).toBe(126);
  });

  /* This is what lifts the tab bar clear of the gesture bar. */
  it("sets --nav-inset-bottom from the shell", () => {
    const { w, props } = fakeWindow({ navBottomPx: () => 126 });
    applyNavInset(w);
    expect(props.get("--nav-inset-bottom")).toBe("48px");
  });

  /* Desktop, the browser build and iOS have no bridge: leave env() alone. */
  it("does nothing where the Android shell is absent", () => {
    const { w, props } = fakeWindow(undefined);
    applyNavInset(w);
    expect(props.size).toBe(0);
  });

  /* A throwing bridge must not break the page. */
  it("falls back quietly if the bridge throws", () => {
    const { w, props } = fakeWindow({
      navBottomPx: () => {
        throw new Error("gone");
      },
    });
    expect(() => applyNavInset(w)).not.toThrow();
    expect(props.size).toBe(0);
  });
});
