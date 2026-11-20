import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { UpdateCheck } from "../api/ps5";
import type { UpdateNotice } from "../lib/updateNotice";
import type { UpdatePhase } from "../state/update";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const { env, seeded, notice } = vi.hoisted(() => ({
  env: { tauri: true, mobile: false },
  seeded: { phase: { kind: "idle" } } as { phase: UpdatePhase },
  notice: { value: { dot: false, row: null, banner: null } } as { value: UpdateNotice },
}));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => env.tauri, safeUnlisten: () => {} }));
vi.mock("../lib/platform", async (orig) => ({
  ...(await orig<typeof import("../lib/platform")>()),
  isMobile: () => env.mobile,
  isAndroid: () => env.mobile,
}));

vi.mock("../state/update", async (orig) => {
  const real = await orig<typeof import("../state/update")>();
  const store = <T,>(sel: (s: Record<string, unknown>) => T) =>
    sel({ ...real.useUpdateStore.getState(), installing: false, skippedVersion: null, lastInstall: null, ...seeded });
  store.getState = real.useUpdateStore.getState;
  return { ...real, useUpdateStore: store };
});

vi.mock("../state/updateNoticeRuntime", async (orig) => ({
  ...(await orig<typeof import("../state/updateNoticeRuntime")>()),
  useUpdateNotice: () => notice.value,
}));

import { UpdateBanner, UpdateBannerView } from "./UpdateBanner";

const result: UpdateCheck = {
  available: true,
  current_version: "12.6.11",
  latest_version: "12.6.13",
  notes: "",
  pub_date: "",
  download_url: "https://github.com/x/PS5Upload-12.6.13-win-x64.zip",
  download_filename: "PS5Upload-12.6.13-win-x64.zip",
};

const render = (el: React.ReactElement) => renderToStaticMarkup(<MemoryRouter>{el}</MemoryRouter>);

afterEach(() => {
  env.tauri = true;
  env.mobile = false;
  seeded.phase = { kind: "idle" };
  notice.value = { dot: false, row: null, banner: null };
});

describe("the update banner", () => {
  it("shows nothing for a routine update: that is a dot on Settings and a row there", () => {
    notice.value = { dot: true, row: { version: "12.6.13", helperNewer: false }, banner: null };
    expect(render(<UpdateBanner />)).toBe("");
  });

  it("a newer helper reads as one line: update this app to match the PS5, with Install and Details", () => {
    seeded.phase = { kind: "available", result };
    const html = render(
      <UpdateBannerView banner={{ kind: "helper-newer", version: "12.6.13", id: "helper-newer:12.6.13->12.6.11", more: 0 }} />,
    );
    expect(html).toContain("Update this app to v12.6.13 to match your PS5");
    expect(html).toContain("Install");
    expect(html).toContain("Details");
    expect(html).toContain("Dismiss");
    // One line, at most 44 px tall, the text cut short rather than wrapped.
    expect(html).toContain("h-11");
    expect(html).toContain("truncate");
    expect(html).not.toContain("flex-wrap");
    // Not the old card's "sending this app's helper would replace it" paragraph.
    expect(html).not.toContain("Another device sent a newer helper");
  });

  it("the web UI says where to update instead of offering Install", () => {
    env.tauri = false;
    const html = render(
      <UpdateBannerView banner={{ kind: "helper-newer", version: "12.6.13", id: "x", more: 0 }} />,
    );
    expect(html).toContain("Update PS5Upload where this page comes from to v12.6.13");
    expect(html).not.toContain(">Install<");
  });

  it("a failed install says so, with Try again on a desktop", () => {
    const html = render(
      <UpdateBannerView banner={{ kind: "install-failed", version: "12.6.13", id: "install-failed:12.6.13", more: 0 }} />,
    );
    expect(html).toContain("Update didn&#x27;t install — v12.6.13");
    expect(html).toContain("Try again");
    expect(html).toContain('data-kind="install-failed"');
  });

  it("a failed install on a phone leaves Try again to Settings (Android's own dialog)", () => {
    env.mobile = true;
    const html = render(
      <UpdateBannerView banner={{ kind: "install-failed", version: "12.6.13", id: "x", more: 0 }} />,
    );
    expect(html).toContain("Update didn&#x27;t install");
    expect(html).not.toContain("Try again");
  });

  it("a second thing needing attention is a count, not a second banner", () => {
    const html = render(
      <UpdateBannerView banner={{ kind: "install-failed", version: "12.6.13", id: "x", more: 1 }} />,
    );
    expect(html).toContain("+1");
    expect(html).toContain("1 more in Settings");
    expect(html.match(/data-testid="update-banner"/g)).toHaveLength(1);
  });
});
