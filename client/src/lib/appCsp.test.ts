import { describe, expect, it } from "vitest";

import confText from "../../src-tauri/tauri.conf.json?raw";

const conf = JSON.parse(confText) as { app: { security: { csp: string; devCsp: string } } };

function directive(csp: string, name: string): string[] {
  const found = csp.split(";").map((d) => d.trim().split(/\s+/)).find(([n]) => n === name);
  return found?.slice(1) ?? [];
}

describe("the app's content security policy", () => {
  /* Settings → Engine URL takes any URL, and the page fetches the engine directly: an engine
     behind a proxy (https) or on another port must not half work. */
  for (const key of ["csp", "devCsp"] as const) {
    it(`${key} lets a remote engine answer on any port and scheme`, () => {
      const csp = conf.app.security[key];
      for (const scheme of ["http:", "https:", "ws:", "wss:"]) {
        expect(directive(csp, "connect-src")).toContain(scheme);
      }
      for (const scheme of ["http:", "https:"]) {
        expect(directive(csp, "img-src")).toContain(scheme);
      }
    });
  }

  /* lib/keepScreenVideo.ts: its clip is a data: URL, and a WebView without the wake lock API has
     nothing else to keep the screen on. */
  it("lets the keep-screen-on clip play", () => {
    expect(directive(conf.app.security.csp, "media-src")).toContain("data:");
  });

  it("still allows scripts from the app alone", () => {
    const csp = conf.app.security.csp;
    expect(directive(csp, "default-src")).toEqual(["'self'"]);
    expect(directive(csp, "script-src")).toEqual([]);
  });
});
