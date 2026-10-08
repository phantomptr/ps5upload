import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", () => ({
  useTr: () =>
    (key: string, vars?: Record<string, string | number>, fallback?: string) => {
      let s = fallback ?? key;
      for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
      return s;
    },
}));

import type { Volume } from "../../api/ps5";
import { StorageCard } from "./index";

const vol = (path: string, over: Partial<Volume> = {}): Volume => ({
  path,
  fs_type: "exfatfs",
  total_bytes: 500e9,
  free_bytes: 400e9,
  writable: true,
  ...over,
});
const html = (v: Volume, packageDrive: string | null) =>
  renderToStaticMarkup(
    <StorageCard volume={v} packageDrive={packageDrive} onUseForPackages={() => {}} />,
  );

describe("StorageCard — default package drive", () => {
  it("marks internal storage as the package drive when none is chosen", () => {
    const out = html(vol("/data", { fs_type: "nullfs" }), null);
    expect(out).toContain("Package copies are stored here");
    expect(out).not.toContain("Store package copies here");
  });

  it("offers a writable USB drive as the package drive", () => {
    const out = html(vol("/mnt/usb0"), null);
    expect(out).toContain("Store package copies here");
    expect(out).not.toContain("Package copies are stored here");
  });

  it("marks the chosen drive and lets internal be picked again", () => {
    expect(html(vol("/mnt/usb0"), "/mnt/usb0")).toContain("Package copies are stored here");
    const internal = html(vol("/data", { fs_type: "nullfs" }), "/mnt/usb0");
    expect(internal).toContain("Store package copies here");
    expect(internal).not.toContain("Package copies are stored here");
  });

  it("shows the console's reserved pool as a fact, and nothing when it is not reported", () => {
    const withKept = renderToStaticMarkup(
      <StorageCard
        volume={vol("/data", { fs_type: "nullfs" })}
        packageDrive={null}
        consoleKeptBytes={66.1e9}
        onUseForPackages={() => {}}
      />,
    );
    expect(withKept).toContain("66.1 GB kept by the console for its own use");
    expect(html(vol("/data", { fs_type: "nullfs" }), null)).not.toContain("kept by the console");
  });

  it("offers nothing on a read-only drive", () => {
    const out = html(vol("/mnt/usb1", { writable: false }), null);
    expect(out).not.toContain("Store package copies here");
  });
});
