import { describe, expect, it } from "vitest";

import { reachFailText, waitingNote, type ReachFail } from "./streamReach";
import { isStreamUnreachableError } from "./networkFix";
import { streamInstallFailHelp } from "./streamInstallHint";

const blocked: ReachFail = {
  stage: "check",
  origin: "http://192.168.1.3:19113",
  cause: "timed_out",
  waited_s: 0,
  source: "host_file",
};

describe("a stream install the PS5 could not start (#942)", () => {
  it("says the PS5 can't connect, names the firewall and its fix, and offers Retry via upload", () => {
    expect(reachFailText(blocked)).toBe(
      "The PS5 can't connect to this computer (192.168.1.3:19113), so it can't fetch the package. " +
        "Most often this computer's firewall is blocking it: allow PS5Upload through the firewall (on Windows, for both Private and Public networks), then press Retry. " +
        "Or press Retry via upload, which copies the package to the PS5 first and needs no connection back.",
    );
  });

  it("a PS5 that was given the package and never fetched says how long it was waited for", () => {
    const text = reachFailText({ ...blocked, stage: "no_fetch", cause: "unknown", waited_s: 60 });
    expect(text).toContain(
      "The PS5 was given the package and hadn't fetched any of it from this computer (192.168.1.3:19113) after 60 seconds, so the install was stopped.",
    );
    expect(text).toContain("firewall");
    expect(text).toContain("Retry via upload");
  });

  it("what the engine found out replaces the usual cause", () => {
    const detail =
      "\u201cEthernet 3\u201d (192.168.1.3), the connection to the PS5, is on a Public network and Windows Firewall lets ps5upload in on Private networks only.";
    const text = reachFailText({ ...blocked, detail });
    expect(text).toContain(detail);
    expect(text).not.toContain("Most often");
    expect(text).toContain("Retry via upload");
  });

  it("a package on a saved server cannot be uploaded instead, so that is not offered", () => {
    const text = reachFailText({ ...blocked, source: "remote" });
    expect(text).toContain("firewall");
    expect(text).not.toContain("Retry via upload");
  });

  it("never names another tool, and never an environment variable", () => {
    for (const r of [blocked, { ...blocked, stage: "no_fetch" as const, waited_s: 60 }]) {
      expect(reachFailText(r)).not.toMatch(/PS5UPLOAD_|curl|netsh/);
    }
  });
});

describe("what a running install says while nothing is moving yet", () => {
  it("names each wait", () => {
    expect(waitingNote("reach_check")).toBe("Checking that the PS5 can reach this computer…");
    expect(waitingNote("console_fetch")).toBe(
      "Waiting for the PS5 to start fetching the package from this computer…",
    );
    expect(waitingNote("previous_install")).toBe(
      "The PS5's installer is still holding on to the last package it couldn't fetch. This install starts by itself when it lets go, in 16 min at most.",
    );
  });

  it("says nothing when the engine names no wait", () => {
    expect(waitingNote(null)).toBeNull();
    expect(waitingNote(undefined)).toBeNull();
  });
});

describe("the failed row's help, in any language", () => {
  const row = {
    status: "failed",
    sourceKind: "install",
    install: { via: "stream", source: "C:\\pkgs\\game.pkg" },
    // A translated message: the English-only check cannot recognise it.
    error: "PS5 无法连接到这台电脑（192.168.1.3:19113），因此无法获取软件包。",
  };

  it("the engine's word that the PS5 could not reach this computer stands in for the wording", () => {
    expect(isStreamUnreachableError(row.error)).toBe(false);
    // Marked by the engine: the row's own message already says it all, so no second hint.
    expect(streamInstallFailHelp({ ...row, streamUnreachable: true })).toEqual({
      retryViaUpload: true,
      hint: false,
    });
    // Not marked: the general hint still shows.
    expect(streamInstallFailHelp(row)).toEqual({ retryViaUpload: true, hint: true });
  });
});
