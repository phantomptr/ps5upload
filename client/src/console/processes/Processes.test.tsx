import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { ApiError } from "../api";
import type { Proc } from "../games/model";
import { SECTIONS } from "../sections";
import { t } from "../strings";
import { canStop, isForeground, ProcessesView, stopErrorText, visibleProcs, type ProcessesViewProps } from "./Processes";
import { systemViewOf } from "../system/SystemSection";

const proc = (over: Partial<Proc>): Proc => ({
  pid: 100,
  name: "x",
  comm: "x",
  titleId: "",
  appId: 0,
  memoryMib: 10,
  threads: 2,
  kind: "system",
  isSelf: false,
  ...over,
});

const PROCS: Proc[] = [
  proc({ pid: 88, comm: "SceShellUI", titleId: "NPXS40087", appId: 5, memoryMib: 300 }),
  proc({ pid: 120, comm: "eboot.bin", titleId: "PPSA01650", appId: 77, kind: "app", memoryMib: 5000 }),
  proc({ pid: 130, comm: "kstuff.elf", kind: "payload", memoryMib: 2 }),
  proc({ pid: 140, comm: "ps5upload.elf", kind: "payload", isSelf: true, memoryMib: 20 }),
];

const base = (over: Partial<ProcessesViewProps> = {}): ProcessesViewProps => ({
  procs: PROCS,
  truncated: false,
  loadError: null,
  showSystem: false,
  foreground: null,
  message: null,
  ask: null,
  busy: false,
  onToggleSystem: vi.fn(),
  onRefresh: vi.fn(),
  onAsk: vi.fn(),
  onConfirm: vi.fn(),
  onDismiss: vi.fn(),
  ...over,
});
const html = (over: Partial<ProcessesViewProps> = {}) => renderToStaticMarkup(<ProcessesView {...base(over)} />);

describe("Processes section", () => {
  it("is System's second view, before Settings in the rail", () => {
    const ids = SECTIONS.map((s) => s.id);
    expect(ids.indexOf("system")).toBeLessThan(ids.indexOf("settings"));
    expect(systemViewOf({ view: "processes" })).toBe("processes");
  });

  it("stops only games and payloads, never the helper or a system process", () => {
    expect(PROCS.map(canStop)).toEqual([false, true, true, false]);
  });

  it("lists games, then payloads, hiding system processes until asked", () => {
    expect(visibleProcs(PROCS, false).map((p) => p.pid)).toEqual([120, 140, 130]);
    expect(visibleProcs(PROCS, true).map((p) => p.pid)).toEqual([120, 140, 130, 88]);
  });

  it("marks the game on screen", () => {
    expect(isForeground(PROCS[1], { title_id: "PPSA01650", app_id: 77 })).toBe(true);
    expect(isForeground(PROCS[0], { title_id: "NPXS40087", app_id: 5 })).toBe(false);
    const out = html({ foreground: { title_id: "PPSA01650", app_id: 77 } });
    expect(out).toContain("is-fg");
    expect(out).toContain(t("cw_games_on_screen"));
  });

  it("shows Stop on stoppable rows only, and the helper as this helper", () => {
    const out = html();
    expect(out).toContain(t("cw_procs_this_helper"));
    expect(out).toMatch(/data-pid="140"[^>]*aria-disabled="true"/);
    expect(out).not.toMatch(/data-pid="120"[^>]*aria-disabled/);
    expect(out).not.toContain("SceShellUI");
  });

  it("asks before a stop, in a game's or a payload's words", () => {
    expect(html({ ask: PROCS[1] })).toContain(t("cw_procs_confirm_game_body"));
    expect(html({ ask: PROCS[2] })).toContain(t("cw_procs_confirm_payload_body"));
  });

  it("explains a refused stop", () => {
    expect(stopErrorText(new ApiError(404, "not_running"))).toBe(t("cw_procs_err_gone"));
    expect(stopErrorText(new ApiError(403, "system"))).toBe(t("cw_procs_err_protected"));
    expect(stopErrorText(new ApiError(403, "helper"))).toBe(t("cw_procs_err_protected"));
    // the kernel said no: in words, not "That did not work (kill_failed)"
    expect(stopErrorText(new ApiError(409, "kill_failed"))).toBe(t("cw_procs_err_refused"));
  });
});
