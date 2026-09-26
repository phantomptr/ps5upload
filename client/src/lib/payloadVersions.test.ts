import { describe, expect, it } from "vitest";
import {
  defaultRelease,
  downloadableReleases,
  isLatestTag,
} from "./payloadVersions";
import type { PayloadReleaseInfo } from "../api/ps5";

function rel(
  tag: string,
  opts: { prerelease?: boolean; asset?: boolean } = {},
): PayloadReleaseInfo {
  return {
    payload_id: "p",
    tag,
    name: tag,
    body: "",
    published_at: "",
    html_url: "",
    picked_asset_url: opts.asset === false ? "" : `https://x/${tag}.elf`,
    picked_asset_name: `${tag}.elf`,
    picked_asset_size: 1,
    prerelease: opts.prerelease ?? false,
    cached_age_secs: 0,
  };
}

describe("downloadableReleases", () => {
  it("drops releases with no matching asset", () => {
    const list = [rel("v3"), rel("v2", { asset: false }), rel("v1")];
    expect(downloadableReleases(list).map((r) => r.tag)).toEqual(["v3", "v1"]);
  });
});

describe("defaultRelease", () => {
  it("picks the newest STABLE release, skipping newer pre-releases", () => {
    // newest-first: a fast-moving pre-release on top, stable below.
    const list = [
      rel("v3.0-rc1", { prerelease: true }),
      rel("v2.9", { prerelease: false }),
      rel("v2.8", { prerelease: false }),
    ];
    expect(defaultRelease(list)?.tag).toBe("v2.9");
  });

  it("falls back to the newest when every release is a pre-release", () => {
    const list = [
      rel("v3.0-rc2", { prerelease: true }),
      rel("v3.0-rc1", { prerelease: true }),
    ];
    expect(defaultRelease(list)?.tag).toBe("v3.0-rc2");
  });

  it("ignores stable releases that have no downloadable asset", () => {
    const list = [
      rel("v3", { prerelease: false, asset: false }), // stable but no asset
      rel("v2", { prerelease: false }),
    ];
    expect(defaultRelease(list)?.tag).toBe("v2");
  });

  it("returns null when nothing is downloadable", () => {
    expect(defaultRelease([])).toBeNull();
    expect(defaultRelease([rel("v1", { asset: false })])).toBeNull();
  });
});

describe("isLatestTag", () => {
  it("is true only for the newest tag", () => {
    const list = [rel("v3"), rel("v2"), rel("v1")];
    expect(isLatestTag(list, "v3")).toBe(true);
    expect(isLatestTag(list, "v2")).toBe(false);
    expect(isLatestTag([], "v3")).toBe(false);
  });
});
