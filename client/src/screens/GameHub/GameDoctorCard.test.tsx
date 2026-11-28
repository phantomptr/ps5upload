import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", async (original) => ({
  ...(await original<typeof import("../../state/lang")>()),
  useTr: () => (key: string, vars?: Record<string, string | number>, fallback?: string) => {
    let s = fallback ?? key;
    for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
    return s;
  },
}));

import type { DoctorCheck } from "../../api/gameDoctor";
import type { Translator } from "../../state/lang";
import { GameDoctorCard, doctorCheckName, doctorCheckText } from "./GameDoctorCard";

const tr: Translator = (key: string, vars?: Record<string, string | number> | string, fallback?: string) => {
  let s = (typeof vars === "string" ? vars : fallback) ?? key;
  if (typeof vars === "object") for (const [k, v] of Object.entries(vars)) s = s.replace(`{${k}}`, String(v));
  return s;
};

describe("Something wrong?", () => {
  it("starts with one Check button and no verdicts", () => {
    const html = renderToStaticMarkup(<GameDoctorCard host="192.168.1.50" titleId="PPSA01234" name="Game" />);
    expect(html).toContain("Something wrong?");
    expect(html).toContain('data-testid="game-doctor-run"');
    expect(html).not.toContain("game-doctor-result");
  });

  it("names each check and words its finding, the engine's English as the fallback", () => {
    const c: DoctorCheck = {
      id: "fake_package",
      status: "unknown",
      reason: "kstuff_version_unknown",
      message: "kstuff is running. On firmware 13.00 ...",
      params: { firmware: "13.00" },
      fix: null,
    };
    expect(doctorCheckName(tr, "fake_package")).toBe("Fake package support (kstuff)");
    expect(doctorCheckText(tr, c)).toBe("kstuff is running. On firmware 13.00 ...");
    expect(doctorCheckText(tr, { ...c, message: "" })).toBe("kstuff_version_unknown");
  });
});
