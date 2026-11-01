import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

import type { LastInstall, UpdateCheck, UpdateDownload } from "../api/ps5";
import type { UpdatePhase } from "../state/update";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

// Under SSR zustand renders a store's initial state, so the toast reads a seeded one instead.
const seeded: { phase: UpdatePhase; lastInstall: LastInstall | null } = {
  phase: { kind: "idle" },
  lastInstall: null,
};
vi.mock("../state/update", async (orig) => {
  const real = await orig<typeof import("../state/update")>();
  return {
    ...real,
    useUpdateStore: <T,>(sel: (s: Record<string, unknown>) => T) =>
      sel({
        ...real.useUpdateStore.getState(),
        ...seeded,
        installing: false,
        skippedVersion: null,
        installOnQuitArmed: false,
      }),
  };
});

import { UpdateToast } from "./UpdateToast";

const result: UpdateCheck = {
  available: true,
  current_version: "10.4.1",
  latest_version: "10.5.1",
  notes: "",
  pub_date: "",
  download_url: "https://github.com/x/PS5Upload-10.5.1-win-x64.zip",
  download_filename: "PS5Upload-10.5.1-win-x64.zip",
};
const download: UpdateDownload = {
  path: "C:\\u\\updates\\PS5Upload-10.5.1-win-x64.zip",
  bytes: 1,
  can_install: true,
};

describe("update banner after a failed install (#611)", () => {
  it("says it didn't install and offers Try again and the download page, not Install", () => {
    seeded.phase = { kind: "downloaded", result, download };
    seeded.lastInstall = { version: "10.5.1", installed: false, message: "x", new: false };
    const html = renderToStaticMarkup(<UpdateToast />);
    expect(html).toContain("Update didn&#x27;t install");
    expect(html).toContain("Try again");
    expect(html).toContain("Download page");
    expect(html).not.toContain("Install and restart");
  });

  it("a ready download still offers Install and restart", () => {
    seeded.phase = { kind: "downloaded", result, download };
    seeded.lastInstall = null;
    const html = renderToStaticMarkup(<UpdateToast />);
    expect(html).toContain("Install and restart");
    expect(html).not.toContain("Try again");
  });

  it("a failure of an older version does not block a newer one", () => {
    seeded.phase = { kind: "available", result };
    seeded.lastInstall = { version: "10.5.0", installed: false, message: "", new: false };
    const html = renderToStaticMarkup(<UpdateToast />);
    expect(html).toContain("Update available");
    expect(html).not.toContain("Try again");
  });
});
