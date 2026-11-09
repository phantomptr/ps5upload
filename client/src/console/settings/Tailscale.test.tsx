import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import en from "../locales/en.json";
import { blockedLines, tailnetError, tailnetLine, TailscaleSection } from "./Tailscale";

describe("console page › Tailscale › turned away", () => {
  it("names each device the list turned away, only while it lists devices", () => {
    const st = {
      enabled: true,
      state: "running",
      allow_mode: "listed" as const,
      blocked_recent: [
        { ip: "100.64.0.7", name: "pixel-9.tail1.ts.net." },
        { ip: "100.64.0.8", name: "" },
      ],
    };
    expect(blockedLines(st)).toEqual([
      { ip: "100.64.0.7", text: "pixel-9 tried to reach this PS5 over Tailscale." },
      { ip: "100.64.0.8", text: "100.64.0.8 tried to reach this PS5 over Tailscale." },
    ]);
    expect(blockedLines({ ...st, allow_mode: "all" })).toEqual([]);
    expect(blockedLines({ ...st, enabled: false })).toEqual([]);
    expect(blockedLines({ enabled: true, state: "running", allow_mode: "listed" })).toEqual([]);
    expect(en.cw_tn_allow).toBe("Allow");
    expect(en.cw_tn_ignore).toBe("Ignore");
  });
});

describe("console page › Tailscale", () => {
  it("says where it stands in words", () => {
    expect(tailnetLine({ enabled: false, state: "off" })).toContain(
      "Turning it off keeps the sign-in",
    );
    expect(
      tailnetLine({
        enabled: true,
        state: "running",
        ip: "100.64.0.7",
        name: "ps5.tail1234.ts.net.",
      }),
    ).toBe("On Tailscale as ps5.tail1234.ts.net at 100.64.0.7");
    expect(
      tailnetLine({
        enabled: true,
        state: "needs-login",
        login_url: "https://login.example/a/1",
      }),
    ).toContain("Scan");
    expect(tailnetLine({ enabled: true, state: "wait-approval" })).toContain(
      "approved",
    );
    expect(tailnetLine({ enabled: true, state: "connecting" })).toBe(
      "Connecting to Tailscale…",
    );
  });

  it("names the lookup a PS5 can't do on its own, and passes other errors through", () => {
    expect(
      tailnetError({
        enabled: true,
        state: "backoff",
        error: "DNS lookup failed",
      }),
    ).toContain("from the PS5Upload app");
    expect(
      tailnetError({
        enabled: true,
        state: "backoff",
        error: "control refused registration",
      }),
    ).toBe("Tailscale: control refused registration");
    expect(tailnetError({ enabled: true, state: "running" })).toBeNull();
  });

  it("starts as a section that reads the helper (nothing to show before it answers)", () => {
    const html = renderToStaticMarkup(<TailscaleSection />);
    // Before the first read it shows nothing: an older helper without the route must not show an error.
    expect(html).toBe("");
  });

  it("has every string it uses in English", () => {
    const keys = [
      "cw_tn_title",
      "cw_tn_sub",
      "cw_tn_switch",
      "cw_tn_off",
      "cw_tn_on_as",
      "cw_tn_connecting",
      "cw_tn_wait_approval",
      "cw_tn_scan",
      "cw_tn_scan_title",
      "cw_tn_scan_body",
      "cw_tn_sign_out",
      "cw_tn_sign_out_confirm",
      "cw_tn_keep",
      "cw_tn_err",
      "cw_tn_err_dns",
      "cw_tn_err_change",
    ];
    for (const k of keys)
      expect((en as Record<string, string>)[k], k).toBeTruthy();
  });
});
