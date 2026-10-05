import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../state/lang", () => ({
  useTr: () => (key: string, vars?: Record<string, string | number>, fallback?: string) => {
    let s = fallback ?? key;
    for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
    return s;
  },
}));

import { FakeGameFirmwareNotice } from "./FakeGameFirmwareNotice";

const K = (fw: string) => `FreeBSD 11.0-RELEASE-p0 #1 r229358/releases/${fw} Jul 17 2026`;
const PS5_GAME = "UP4433-PPSA17221_00-MINECRAFTPS50000";

const html = (kernel: string | null, id: string | null, compact = false) =>
  renderToStaticMarkup(<FakeGameFirmwareNotice kernel={kernel} contentId={id} compact={compact} />);

describe("FakeGameFirmwareNotice", () => {
  it("says it installs but isn't playable, for a PS5 game on 13.60", () => {
    const out = html(K("13.60"), PS5_GAME);
    expect(out).toContain("can be installed but not played on FW 13.60");
    expect(out).toContain("It will install, but PS5 fake game packages can&#x27;t be played on firmware above 11.60");
    expect(out).toContain("PS4 packages are fine");
  });
  it("renders the one-line form for a list row", () => {
    expect(html(K("11.61"), PS5_GAME, true)).toContain("above 11.60");
  });
  it("renders nothing for 11.60, PS4, homebrew or an unknown firmware", () => {
    expect(html(K("11.60"), PS5_GAME)).toBe("");
    expect(html(K("13.60"), "UP9000-CUSA00207_00-BLOODBORNE000000")).toBe("");
    expect(html(K("13.60"), "IV0002-ITEM00001_00-ITEMZFLOWIV00000")).toBe("");
    expect(html("unknown build", PS5_GAME)).toBe("");
    expect(html(null, PS5_GAME)).toBe("");
  });
});
