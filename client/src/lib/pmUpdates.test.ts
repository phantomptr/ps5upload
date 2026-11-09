import { describe, expect, it } from "vitest";

import type { PayloadReleaseInfo } from "../api/ps5";
import { catalogueMatch, isRepoManaged, pmUpdateFor, slotStem, swapAutoloadKey, tagIsNewer } from "./pmUpdates";

const rel = (tag: string, url = "https://x/a.elf"): PayloadReleaseInfo => ({
  payload_id: "nanodns",
  tag,
  name: tag,
  body: "",
  published_at: "",
  html_url: "",
  picked_asset_url: url,
  picked_asset_name: "a.elf",
  picked_asset_size: 1,
  cached_age_secs: 0,
});

describe("pmUpdateFor", () => {
  it("offers the newer release only for something this app stored", () => {
    expect(pmUpdateFor({ id: "nanodns", tag: "v0.4" }, rel("v0.5"))?.tag).toBe("v0.5");
    expect(pmUpdateFor({ id: "nanodns", tag: "v0.5" }, rel("v0.5"))).toBeNull();
    expect(pmUpdateFor(undefined, rel("v0.5"))).toBeNull();
    expect(pmUpdateFor({ id: "nanodns", tag: "v0.4" }, rel("v0.5", ""))).toBeNull();
  });
});

describe("swapAutoloadKey", () => {
  it("keeps the boot position and the pauses", () => {
    expect(swapAutoloadKey(["a.elf", "!3000", "old.elf", "!3000", "c.elf"], "old.elf", "new.elf")).toEqual([
      "a.elf",
      "!3000",
      "new.elf",
      "!3000",
      "c.elf",
    ]);
  });
  it("does not list the new one twice", () => {
    expect(swapAutoloadKey(["new.elf", "old.elf"], "old.elf", "new.elf")).toEqual(["new.elf"]);
  });
  it("leaves a list without the old one alone", () => {
    const l = ["a.elf"];
    expect(swapAutoloadKey(l, "old.elf", "new.elf")).toBe(l);
  });
});

describe("isRepoManaged", () => {
  it("knows Payload Manager's own repository installs", () => {
    expect(isRepoManaged({ slot: "", path: "/data/pldmgr/payloads/repo/nanodns@e8cce5d1/nanoDNS_0.4.elf" })).toBe(true);
    expect(isRepoManaged({ slot: "repo/nanodns@x", path: "/data/pldmgr/payloads/repo/x" })).toBe(true);
    expect(isRepoManaged({ slot: "local/nanodns", path: "/data/pldmgr/payloads/local/nanodns/n.elf" })).toBe(false);
  });
});

describe("catalogueMatch", () => {
  const inv = [
    { payload_id: "kstuff", path: "/cache/kstuff/1.6/kstuff.elf" },
    { payload_id: "nanodns", path: "C:\\cache\\nanodns\\nanoDNS_0.4.elf" },
  ];
  it("matches a hand-stored payload to the catalogue file with the same name", () => {
    expect(catalogueMatch({ slot: "local/kstuff", path: "/data/pldmgr/payloads/local/kstuff/kstuff.elf", filename: "kstuff.elf" }, inv)).toBe("kstuff");
    expect(catalogueMatch({ slot: "", path: "/data/pldmgr/payloads/nanodns/NANODNS_0.4.ELF", filename: "NANODNS_0.4.ELF" }, inv)).toBe("nanodns");
  });
  it("leaves repository payloads, USB finds and unknown files alone", () => {
    expect(catalogueMatch({ slot: "repo/kstuff@x", path: "/data/pldmgr/payloads/repo/kstuff@x/kstuff.elf", filename: "kstuff.elf" }, inv)).toBeNull();
    expect(catalogueMatch({ slot: "", path: "/mnt/usb0/kstuff.elf", filename: "kstuff.elf" }, inv)).toBeNull();
    expect(catalogueMatch({ slot: "local/x", path: "/data/pldmgr/payloads/local/x/x.elf", filename: "x.elf" }, inv)).toBeNull();
  });
});

describe("tagIsNewer (the engine's is_newer)", () => {
  it("orders numbers, then a pre-release before its release before a lettered revision", () => {
    expect(tagIsNewer("v1.10", "v1.9")).toBe(true);
    expect(tagIsNewer("1.2", "v1.2")).toBe(false);
    // ShadowMount+'s betas of one version are each an update; a newer pre-release is kept.
    expect(tagIsNewer("1.7beta11", "1.7beta10")).toBe(true);
    expect(tagIsNewer("1.7beta11", "1.7beta12")).toBe(false);
    expect(tagIsNewer("1.7", "1.7beta12")).toBe(true);
    expect(tagIsNewer("v1.15-beta16", "v1.15-beta15")).toBe(true);
    expect(tagIsNewer("1.2-dr-test2", "1.2-dr-test1")).toBe(true);
    expect(tagIsNewer("1.0", "1.0b3")).toBe(true);
    expect(tagIsNewer("2.5B", "2.5")).toBe(true);
    expect(tagIsNewer("2.5C", "2.5B")).toBe(true);
    expect(tagIsNewer("2.5", "2.5B")).toBe(false);
    expect(tagIsNewer("latest", "nightly")).toBe(true);
    expect(tagIsNewer("v1", "")).toBe(true);
    expect(tagIsNewer("", "v1")).toBe(false);
  });
  it("never offers a stored pre-release's older release as an update", () => {
    expect(pmUpdateFor({ id: "shadowmountplus", tag: "1.7beta12" }, rel("1.7beta11"))).toBeNull();
    expect(pmUpdateFor({ id: "shadowmountplus", tag: "1.7beta10" }, rel("1.7beta11"))?.tag).toBe("1.7beta11");
  });
});

describe("matching a hand-stored copy to the catalogue by name", () => {
  const catalog = [
    { id: "shadowmountplus", file_name: "shadowmountplus.elf" },
    { id: "kstuff-echostretch", file_name: "kstuff.elf" },
    { id: "kstuff-lite-drakmor", file_name: "kstuff.elf" },
    { id: "ftpsrv", file_name: "ftpsrv.elf" },
    { id: "payload-manager", file_name: "pldmgr.elf" },
    { id: "custom-1", file_name: "", is_custom: true },
  ];
  const at = (filename: string) => ({ slot: "", path: `/data/pldmgr/payloads/local/x/${filename}`, filename });
  it("keys a file as Payload Manager 0.6 does", () => {
    expect(slotStem("shadowmountplus_v1.2.elf")).toBe("shadowmountplus");
    expect(slotStem("ShadowMountPlus-1.7beta11.ELF")).toBe("shadowmountplus");
    expect(slotStem("ftpsrv-ps5.elf")).toBe("ftpsrv");
    expect(slotStem("np-fake-signin-ps5.elf")).toBe("np-fake-signin");
    expect(slotStem("ps5upload.elf")).toBe("ps5upload");
  });
  it("finds the catalogue payload when it was never downloaded here, only when certain", () => {
    expect(catalogueMatch(at("shadowmountplus_v1.2.elf"), [], catalog)).toBe("shadowmountplus");
    expect(catalogueMatch(at("ftpsrv-ps5.elf"), [], catalog)).toBe("ftpsrv");
    // Two catalogue payloads share kstuff.elf: neither is guessed.
    expect(catalogueMatch(at("kstuff.elf"), [], catalog)).toBeNull();
    expect(catalogueMatch(at("pldmgr_v0.6.3.elf"), [], catalog)).toBeNull();
    expect(catalogueMatch(at("mything.elf"), [], catalog)).toBeNull();
    // A download here still wins.
    expect(
      catalogueMatch(at("kstuff.elf"), [{ payload_id: "kstuff-echostretch", path: "/c/kstuff.elf" }], catalog),
    ).toBe("kstuff-echostretch");
  });
});
