import { describe, expect, it } from "vitest";
import { applyNavInset, navInsetCssPx } from "./androidInsets";

function fakeWindow(bridge?: { systemInsetsPx(): string }, dpr = 2.625) {
  const props = new Map<string, string>();
  const attrs = new Set<string>();
  const w = {
    devicePixelRatio: dpr,
    PS5UploadInsets: bridge,
    document: {
      documentElement: {
        style: { setProperty: (k: string, v: string) => void props.set(k, v) },
        toggleAttribute: (name: string, on: boolean) => {
          if (on) attrs.add(name);
          else attrs.delete(name);
        },
      },
    },
  } as unknown as Window;
  return { w, props, attrs };
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
    const { w, props } = fakeWindow({ systemInsetsPx: () => "0,0,126,0" });
    applyNavInset(w);
    expect(props.get("--nav-inset-bottom")).toBe("48px");
  });

  /* A device with no cutout, and a 3-button nav bar on the side of a phone held sideways:
     neither reaches env() on a cutout-only WebView. */
  it("sets the other sides from the shell too", () => {
    const { w, props } = fakeWindow({ systemInsetsPx: () => "63,126,0,42" });
    applyNavInset(w);
    expect(props.get("--sys-inset-top")).toBe("24px");
    expect(props.get("--sys-inset-right")).toBe("48px");
    expect(props.get("--nav-inset-bottom")).toBe("0px");
    expect(props.get("--sys-inset-left")).toBe("16px");
  });

  /* The tab bar and activity pill go while the keyboard is up (index.css). */
  it("marks the page while the keyboard is up", () => {
    let insets = "63,0,0,0,1008";
    const { w, attrs } = fakeWindow({ systemInsetsPx: () => insets });
    applyNavInset(w);
    expect(attrs.has("data-keyboard")).toBe(true);
    insets = "63,0,126,0,0";
    applyNavInset(w);
    expect(attrs.has("data-keyboard")).toBe(false);
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
      systemInsetsPx: () => {
        throw new Error("gone");
      },
    });
    expect(() => applyNavInset(w)).not.toThrow();
    expect(props.size).toBe(0);
  });
});
