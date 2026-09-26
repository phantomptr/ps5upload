import { describe, expect, it } from "vitest";
import en from "../i18n/locales/en";
import {
  dpiUnavailableCopy,
  patchInstallFailure,
  PKG_PATCH_DAEMON_NO_BRINGUP_HINT,
  PKG_PATCH_DAEMON_UNAVAILABLE_HINT,
  PKG_PATCH_LOADER_UNREACHABLE_HINT,
  PKG_PATCH_LOADER_NOT_LISTENING_HINT,
} from "./dpiUnavailable";

describe("dpiUnavailableCopy", () => {
  it("blames the console's loader when :9021 refused the connection", () => {
    // The 2026-09-08 report: released Docker engine, image in hand, and
    // `connect …:9021: Connection refused`. The old single message sent this
    // user to rebuild an engine that was already correct.
    const copy = dpiUnavailableCopy("loader_unreachable");
    expect(copy.text).toBe(PKG_PATCH_LOADER_UNREACHABLE_HINT);
    expect(copy.text).toContain("9021");
    expect(copy.text).not.toContain("Docker");
  });

  it("gives the same loader advice when the send broke mid-transfer", () => {
    expect(dpiUnavailableCopy("loader_send_failed").text).toBe(
      PKG_PATCH_LOADER_UNREACHABLE_HINT,
    );
  });

  it("blames the build only when the build really has no image", () => {
    const copy = dpiUnavailableCopy("no_image");
    expect(copy.text).toBe(PKG_PATCH_DAEMON_UNAVAILABLE_HINT);
    expect(copy.text).toContain("ps5upload-engine Docker image");
  });

  it("reports a daemon that was sent but never answered", () => {
    const copy = dpiUnavailableCopy("no_bringup");
    expect(copy.text).toBe(PKG_PATCH_DAEMON_NO_BRINGUP_HINT);
    expect(copy.text).toContain("9040");
  });

  it("falls back to the neutral message rather than guessing", () => {
    // An older engine sends no reason at all. Guessing here is what produced
    // the wrong advice in the first place, so the fallback must not name a
    // cause: no engine-build blame, no loader blame.
    for (const unknown of [undefined, null, "", "something_new"]) {
      const copy = dpiUnavailableCopy(unknown);
      expect(copy.text).toBe(PKG_PATCH_DAEMON_NO_BRINGUP_HINT);
      expect(copy.text).not.toContain("payload SDK");
      expect(copy.text).not.toContain("9021");
    }
  });

  it("keeps every message honest about the base game and the manual route", () => {
    for (const r of ["no_image", "loader_unreachable", "no_bringup"]) {
      const { text } = dpiUnavailableCopy(r);
      expect(text).toContain("Your base game is untouched.");
      expect(text).toContain("Package Installer");
      // Every one of these failures keeps the staged pkg. Saying so is what
      // stops a user re-sending a 6 GB file that is already on the console.
      expect(text).toContain("nothing needs uploading again");
    }
  });

  it("uses a distinct i18n key per cause", () => {
    const keys = ["no_image", "loader_unreachable", "no_bringup"].map(
      (r) => dpiUnavailableCopy(r).key,
    );
    expect(new Set(keys).size).toBe(3);
  });

  it("matches the English text shipped in en.ts", () => {
    // trStatic falls back to the constant when a key is missing, so a locale
    // drift is invisible at runtime — every user would silently keep reading
    // the fallback. Pin them together instead.
    for (const reason of ["no_image", "loader_unreachable", "no_bringup"]) {
      const { key, text } = dpiUnavailableCopy(reason);
      expect((en as Record<string, string>)[key], key).toBe(text);
    }
  });
});

describe("patchInstallFailure", () => {
  // The 2026-09-09 reports: FW 11.00, four bundles, all identical. The staged
  // PS4 update was rejected in-process with 0x80B2116F, the DPI fallback could
  // not be delivered because :9021 refused, and the message the user was left
  // with mentioned only the daemon.
  const MAIN = "0x80b2116f";

  it("keeps the reason the install was actually rejected", () => {
    // The DP branch dropped mainErr entirely, so the code that names the real
    // problem — and its Debug Settings remedy — never reached the user or the
    // next bug report. The non-DP branch had always included it.
    const msg = patchInstallFailure({
      mainErr: MAIN,
      reason: "loader_unreachable",
      dpiErr: "send dpi.elf: connect 10.0.0.5:9021: Connection refused",
    });
    expect(msg).toContain(MAIN);
    expect(msg).toContain("9021");
  });

  it("does not tell someone to re-run a loader that is already running", () => {
    // elfldr.elf was in the process list of all four bundles while :9021
    // refused. "Re-run your loader" reads as wrong to someone looking at a
    // running loader, which is why this user concluded the app could not help
    // them and stopped.
    const running = patchInstallFailure({
      mainErr: MAIN,
      reason: "loader_unreachable",
      dpiErr: "connect: refused",
      loaderProcessRunning: true,
    });
    expect(running).toMatch(/running but is not accepting|not listening/i);
    expect(running).not.toMatch(/re-run the loader on your PS5/i);

    const absent = patchInstallFailure({
      mainErr: MAIN,
      reason: "loader_unreachable",
      dpiErr: "connect: refused",
      loaderProcessRunning: false,
    });
    expect(absent).toMatch(/re-run/i);
  });

  it("always offers the console-side route, which needs no loader at all", () => {
    for (const reason of ["no_image", "loader_unreachable", "no_bringup"]) {
      const msg = patchInstallFailure({ mainErr: MAIN, reason, dpiErr: "x" });
      expect(msg, reason).toContain("Package Installer");
    }
  });

  it("says the upload is not wasted", () => {
    // A non-confirmed install keeps its staged pkg; people were re-uploading
    // gigabytes that were already on the console.
    const msg = patchInstallFailure({ mainErr: MAIN, reason: "no_bringup", dpiErr: "x" });
    expect(msg).toMatch(/already on your PS5/i);
  });

  it("works when there was no primary error to report", () => {
    const msg = patchInstallFailure({ reason: "loader_unreachable", dpiErr: "x" });
    expect(msg).toContain("9021");
    expect(msg).not.toContain("undefined");
  });
});

describe("the new copy is translatable", () => {
  it("registers pkg.patch_loader_not_listening in en.ts, matching the constant", () => {
    // The key is chosen at runtime, so the i18n gate cannot see it — it only
    // scans literal tr("…") calls. Without this test the string would silently
    // stay English in all 19 locales.
    const table = en as unknown as Record<string, string>;
    expect(table["pkg.patch_loader_not_listening"]).toBe(PKG_PATCH_LOADER_NOT_LISTENING_HINT);
  });
});
