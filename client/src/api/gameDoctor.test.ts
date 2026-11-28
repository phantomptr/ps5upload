import { describe, expect, it } from "vitest";

import en from "../i18n/locales/en";
import { normalizeDoctorAnswer } from "./gameDoctor";

// What the engine answers for a folder game whose eboot.bin is 0644 (a folder copied from
// Windows): the first problem is permissions, with the fix the card offers.
const answer = {
  ok: true,
  title_id: "PPSA01234",
  kind: "folder",
  source: "/data/homebrew/PPSA01234",
  firmware: "13.00",
  first_problem: "permissions",
  checks: [
    { id: "fake_package", status: "pass", reason: "not_a_package", message: "m" },
    { id: "mount_layer", status: "pass", reason: "mounter_attaches", message: "m", params: { mounter: "ShadowMount+" } },
    { id: "package_size", status: "pass", reason: "not_a_package", message: "m" },
    { id: "database", status: "unknown", reason: "db_unreadable", message: "m" },
    {
      id: "permissions",
      status: "problem",
      reason: "unreadable",
      message: "m",
      params: { count: 1, checked: 9, path: "/data/homebrew/PPSA01234/eboot.bin", mode: "0644" },
      fix: "fix_permissions",
    },
    { id: "free_space", status: "pass", reason: "ok", message: "m", params: { free_gb: "120.0" } },
    { id: "something_new", status: "problem", reason: "x", message: "m" },
  ],
};

describe("the engine's doctor answer", () => {
  it("keeps the checks in order and drops what the app doesn't know", () => {
    const a = normalizeDoctorAnswer(answer);
    expect(a.kind).toBe("folder");
    expect(a.source).toBe("/data/homebrew/PPSA01234");
    expect(a.firstProblem).toBe("permissions");
    expect(a.checks.map((c) => c.id)).toEqual([
      "fake_package",
      "mount_layer",
      "package_size",
      "database",
      "permissions",
      "free_space",
    ]);
    const p = a.checks[4];
    expect(p.fix).toBe("fix_permissions");
    expect(p.params).toEqual({ count: 1, checked: 9, path: "/data/homebrew/PPSA01234/eboot.bin", mode: "0644" });
  });

  it("reads anything it can't trust as unknown, never as a pass", () => {
    const a = normalizeDoctorAnswer({
      kind: "laser",
      first_problem: "nope",
      checks: [{ id: "database", status: "fine", reason: 3, fix: "reboot" }],
    });
    expect(a.kind).toBe("unknown");
    expect(a.firstProblem).toBeNull();
    expect(a.checks).toEqual([
      { id: "database", status: "unknown", reason: "", message: "", params: {}, fix: null },
    ]);
    expect(normalizeDoctorAnswer(null).checks).toEqual([]);
  });

  it("has English for every reason in the sample", () => {
    const dict = en as Record<string, string>;
    for (const c of normalizeDoctorAnswer(answer).checks) {
      expect(dict[`doctor_${c.id}_${c.reason}`], `${c.id}/${c.reason}`).toBeTruthy();
      expect(dict[`doctor_name_${c.id}`], c.id).toBeTruthy();
    }
  });
});
