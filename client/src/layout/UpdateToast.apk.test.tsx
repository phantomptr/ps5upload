import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { UpdateCheck } from "../api/ps5";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const result: UpdateCheck = {
  available: true,
  current_version: "11.0.8",
  latest_version: "11.0.9",
  notes: "",
  pub_date: "",
  download_url: "https://github.com/x/PS5Upload-11.0.9-android.apk",
  download_filename: "PS5Upload-11.0.9-android.apk",
};

const platform = { android: true, ios: false };
vi.mock("../lib/platform", async (orig) => ({
  ...(await orig<typeof import("../lib/platform")>()),
  isMobile: () => platform.android || platform.ios,
  isAndroid: () => platform.android,
  isIOS: () => platform.ios,
}));

const seededStep: { step: unknown } = { step: { kind: "idle" } };
vi.mock("../state/apkUpdate", async (orig) => {
  const real = await orig<typeof import("../state/apkUpdate")>();
  return {
    ...real,
    apkUpdaterAvailable: () => platform.android,
    useApkUpdateStore: <T,>(sel: (s: { step: unknown }) => T) => sel(seededStep),
  };
});

// Back to "available", as `startInstall` leaves it once Android has the APK.
vi.mock("../state/update", async (orig) => {
  const real = await orig<typeof import("../state/update")>();
  return {
    ...real,
    useUpdateStore: <T,>(sel: (s: Record<string, unknown>) => T) =>
      sel({
        ...real.useUpdateStore.getState(),
        phase: { kind: "available", result },
        installing: false,
        skippedVersion: null,
        lastInstall: null,
      }),
  };
});

import { UpdateToast } from "./UpdateToast";

afterEach(() => {
  platform.android = true;
  platform.ios = false;
  seededStep.step = { kind: "idle" };
});

describe("the update toast on a phone", () => {
  it("says why Android refused, with the download page, instead of going back to Install", () => {
    seededStep.step = { kind: "failed", message: "App not installed: blocked by Play Protect" };
    const html = renderToStaticMarkup(<UpdateToast />);
    expect(html).toContain("Update didn&#x27;t install");
    expect(html).toContain("blocked by Play Protect");
    expect(html).toContain("Download page");
    expect(html).not.toContain("Update available");
  });

  it("lets the row wrap on a phone, so a hint is not cut off", () => {
    const html = renderToStaticMarkup(<UpdateToast />);
    expect(html).toContain("max-sm:flex-wrap");
    expect(html).toContain("max-sm:whitespace-normal");
  });

  it("tells an iPhone what comes after the download", () => {
    platform.android = false;
    platform.ios = true;
    const html = renderToStaticMarkup(<UpdateToast />);
    expect(html).toContain("sideloading app");
  });

  it("says nothing about sideloading on Android", () => {
    expect(renderToStaticMarkup(<UpdateToast />)).not.toContain("sideloading");
  });
});
