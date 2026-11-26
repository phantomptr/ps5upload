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
    expect(withKept).toContain(
      "61.6 GiB is held back by the PS5 for itself. It is not free space, and it grows as data is written.",
    );
    expect(html(vol("/data", { fs_type: "nullfs" }), null)).not.toContain("held back by the PS5");
  });

  // #944: a 2 TB console with 175 GB free and 308 GB held back. Free is shown as it is (the
  // held-back space is already out of it), what is likely to fit is said for what it is, and the
  // card does not claim to match the PS5's own Storage screen.
  it("says which number means room, and that the PS5's own screen can show less", () => {
    const internal = vol("/data", {
      fs_type: "nullfs",
      total_bytes: 1995416338432,
      free_bytes: 175467069440,
      safety_reserve_bytes: 1073741824,
      allocatable_bytes: 174393327616,
      likely_fits_bytes: 144432988160,
    });
    const out = renderToStaticMarkup(
      <StorageCard
        volume={internal}
        packageDrive={null}
        consoleKeptBytes={307929022464}
        onUseForPackages={() => {}}
      />,
    );
    expect(out).toContain("163 GiB");
    expect(out).toContain("287 GiB is held back by the PS5 for itself.");
    expect(out).toContain("About 135 GiB of new data is likely to fit here.");
    expect(out).toContain("The PS5&#x27;s own Storage screen can show less free space.");
    // GiB throughout: no decimal GB beside it.
    expect(out).not.toMatch(/\d GB\b/);
    // No longer worded as if the gap were all held back by the system.
    expect(out).not.toContain("headroom");
  });

  it("says nothing about the PS5's screen on a drive that is not internal storage", () => {
    const out = html(vol("/mnt/ext0", { allocatable_bytes: 300e9, likely_fits_bytes: 300e9 }), null);
    expect(out).not.toContain("Storage screen");
  });

  it("offers nothing on a read-only drive", () => {
    const out = html(vol("/mnt/usb1", { writable: false }), null);
    expect(out).not.toContain("Store package copies here");
  });
});

describe("StorageCard — Free up space", () => {
  const card = (v: Volume, onFreeUp?: () => void) =>
    renderToStaticMarkup(
      <StorageCard volume={v} packageDrive={"/mnt/usb0"} onUseForPackages={() => {}} onFreeUp={onFreeUp} />,
    );

  it("offers it when the screen gives one, and not otherwise", () => {
    expect(card(vol("/mnt/ext0"), () => {})).toContain("Free up space");
    expect(card(vol("/mnt/ext0"))).not.toContain("Free up space");
  });

  it("makes it the main action once the drive is 80% full", () => {
    const full = card(vol("/mnt/ext0", { free_bytes: 50e9 }), () => {});
    const roomy = card(vol("/mnt/ext0"), () => {});
    const freeUpButton = (h: string) => /<button[^>]*>(?:(?!<\/button>).)*Free up space/.exec(h)?.[0] ?? "";
    expect(freeUpButton(full)).not.toEqual(freeUpButton(roomy));
  });
});
