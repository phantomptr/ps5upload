import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../api", async (importOriginal) => {
  const real = await importOriginal<typeof import("../api")>();
  return { ...real, call: vi.fn(), callBlob: vi.fn() };
});

import { ApiError, call } from "../api";
import { t } from "../strings";
import { busyText, sheetActions } from "./Files";
import type { OpStatus } from "./opJob";
import {
  installCauseText,
  installMessage,
  installPhaseText,
  installStoppable,
  isPayload,
  isPkg,
  loadPayload,
  payloadErrorText,
  startInstall,
  type PkgInfo,
} from "./pkg";
import { installBlocker, PkgDialogView, pkgRows } from "./PkgDialog";
import type { Entry } from "./useDir";

const entry = (name: string, kind: Entry["kind"] = "file", size = 10): Entry => ({ name, kind, size, mtime: 0 });

const INFO: PkgInfo = {
  title: "Jak X: Combat Racing",
  title_id: "CUSA07842",
  content_id: "UP9000-CUSA07842_00-SCUSJAKXPS4PORT0",
  version: "01.02",
  category: "gd",
  kind: "game",
  platform: "ps4",
  fw: "5.05",
  size: 3_600_000_000,
  fake: false,
  icon: true,
  installed: false,
  image_mounted: false,
  staged: false,
  installing: false,
};

const status = (over: Partial<OpStatus>): OpStatus => ({
  job: "j",
  state: "failed",
  files_done: 0,
  files_total: 0,
  bytes_done: 0,
  bytes_total: 0,
  message: null,
  result: null,
  code: null,
  ...over,
});

const noFallback = () => null;

describe("which files are packages and payloads", () => {
  it("matches .pkg, .elf and .bin in any case", () => {
    expect(isPkg("Game.PKG")).toBe(true);
    expect(isPkg("game.pkg.part")).toBe(false);
    expect(isPayload("kstuff.elf")).toBe(true);
    expect(isPayload("payload.BIN")).toBe(true);
    expect(isPayload("notes.txt")).toBe(false);
  });

  it("offers Install for a .pkg and Load payload for a .elf, first in the sheet", () => {
    const entries = [entry("a.pkg"), entry("k.elf"), entry("x.txt")];
    expect(sheetActions(["a.pkg"], entries, null)[0]).toBe("install");
    expect(sheetActions(["k.elf"], entries, null)[0]).toBe("loadpayload");
    expect(sheetActions(["x.txt"], entries, null)).not.toContain("install");
    expect(sheetActions(["x.txt"], entries, null)).not.toContain("loadpayload");
    // a folder named like a package is not one
    expect(sheetActions(["d.pkg"], [entry("d.pkg", "dir")], null)).not.toContain("install");
  });
});

describe("the helper calls", () => {
  it("asks for replace only when reinstalling", async () => {
    vi.mocked(call).mockResolvedValue({ job: "abc" });
    await expect(startInstall("/data/a.pkg", false)).resolves.toBe("abc");
    expect(call).toHaveBeenLastCalledWith("/api/pkg/install", { path: "/data/a.pkg" });
    await startInstall("/data/a.pkg", true);
    expect(call).toHaveBeenLastCalledWith("/api/pkg/install", { path: "/data/a.pkg", replace: true });
    vi.mocked(call).mockResolvedValue({});
    await expect(startInstall("/data/a.pkg", false)).rejects.toBeInstanceOf(ApiError);
  });

  it("loads a payload by path", async () => {
    vi.mocked(call).mockResolvedValue({ bytes: 1234 });
    await expect(loadPayload("/data/k.elf")).resolves.toBe(1234);
    expect(call).toHaveBeenLastCalledWith("/api/payload/load", { path: "/data/k.elf" });
  });
});

describe("install progress and outcome", () => {
  it("names the phase from the job message", () => {
    expect(installPhaseText("pkg_install_staging")).toBe(t("cw_pkg_phase_staging"));
    expect(installPhaseText("pkg_install_installing")).toBe(t("cw_pkg_phase_installing"));
    expect(installPhaseText("something else")).toBeNull();
  });

  it("lets Stop act only while the package is copied", () => {
    expect(installStoppable(null)).toBe(true);
    expect(installStoppable("pkg_install_staging")).toBe(true);
    expect(installStoppable("pkg_install_installing")).toBe(false);
    expect(installStoppable("pkg_install_finishing")).toBe(false);
  });

  it("shows the install's line with its percentage in the batch status", () => {
    const line = busyText({
      op: "install",
      at: 1,
      total: 1,
      stopping: false,
      paths: ["/mnt/usb0/a.pkg"],
      phase: "pkg_install_staging",
      bytesDone: 50,
      bytesTotal: 200,
    });
    expect(line).toContain("a.pkg");
    expect(line).toContain(t("cw_pkg_phase_staging"));
    expect(line).toContain(t("cw_home_percent", { n: 25 }));
  });

  it("says installed or handed over, by the job's verdict", () => {
    const done = (verdict: string) => installMessage({ end: "done", status: status({ state: "done", result: JSON.stringify({ verdict }) }) }, "Jak X", noFallback);
    expect(done("installed")).toEqual({ tone: "info", text: t("cw_pkg_done_installed", { name: "Jak X" }) });
    expect(done("delivered")).toEqual({ tone: "info", text: t("cw_pkg_done_delivered", { name: "Jak X" }) });
  });

  it("explains a refusal with its code and a cancel as stopped", () => {
    const failed = installMessage({ end: "failed", status: status({ message: "pkg_install_refused: 0x80B2116F" }) }, "x", noFallback);
    expect(failed?.tone).toBe("error");
    expect(failed?.text).toContain("0x80B2116F");
    const cancelled = installMessage({ end: "failed", status: status({ message: "pkg_install_cancelled" }) }, "x", noFallback);
    expect(cancelled).toEqual({ tone: "info", text: t("cw_pkg_stopped") });
    const unknown = installMessage({ end: "failed", status: status({ message: "pkg_install_weird" }) }, "x", noFallback);
    expect(unknown?.text).toContain("pkg_install_weird");
  });

  it("maps the start's refusals", () => {
    expect(installCauseText("pkg_install_busy")).toBe(t("cw_pkg_err_busy"));
    expect(installCauseText("convert_busy")).toBe(t("cw_convert_err_busy"));
    expect(installCauseText("pkg_install_image_mounted")).toBe(t("cw_pkg_err_image"));
    expect(installCauseText("loader_unreachable")).toBe(t("cw_pkg_err_loader"));
    expect(installCauseText("pkg_not_pkg")).toBe(t("cw_pkg_err_not_pkg"));
    const started = installMessage({ end: "error", error: new ApiError(409, "pkg_install_already_installed") }, "x", noFallback);
    expect(started?.text).toContain(t("cw_pkg_err_installed"));
  });

  it("maps payload refusals", () => {
    expect(payloadErrorText(new ApiError(409, "payload_is_helper"), noFallback)).toBe(t("cw_payload_err_helper"));
    expect(payloadErrorText(new ApiError(409, "pkg_install_busy"), noFallback)).toBe(t("cw_payload_err_installing"));
    expect(payloadErrorText(new ApiError(400, "payload_not_elf"), noFallback)).toBe(t("cw_payload_err_not_elf"));
    expect(payloadErrorText(new ApiError(502, "loader_unreachable"), noFallback)).toBe(t("cw_pkg_err_loader"));
  });
});

describe("the package preview", () => {
  it("lists what the package says", () => {
    const rows = pkgRows(INFO);
    const map = new Map(rows);
    expect(map.get(t("cw_pkg_title_id"))).toBe("CUSA07842");
    expect(map.get(t("cw_pkg_version"))).toBe("01.02");
    expect(map.get(t("cw_pkg_type"))).toBe(`${t("cw_pkg_kind_game")} · PS4`);
    expect(map.get(t("cw_pkg_fw"))).toBe("5.05");
    expect(new Map(pkgRows({ ...INFO, fw: "" })).get(t("cw_pkg_fw"))).toBe(t("cw_pkg_fw_unknown"));
  });

  it("holds Install for a mounted image, a running install or another batch", () => {
    expect(installBlocker(INFO, false)).toBeNull();
    expect(installBlocker({ ...INFO, image_mounted: true }, false)).toBe(t("cw_pkg_err_image"));
    expect(installBlocker({ ...INFO, installing: true }, false)).toBe(t("cw_pkg_err_busy"));
    expect(installBlocker(INFO, true)).not.toBeNull();
  });

  it("offers Install, or Reinstall with the warning for an installed game", () => {
    const fresh = renderToStaticMarkup(
      <PkgDialogView name="a.pkg" state={{ info: INFO }} icon="blob:x" busy={false} onInstall={vi.fn()} onClose={vi.fn()} />,
    );
    expect(fresh).toContain(INFO.title);
    expect(fresh).toContain('src="blob:x"');
    expect(fresh).toContain(t("cw_pkg_install"));
    expect(fresh).not.toContain("data-pkg-installed");
    const installed = renderToStaticMarkup(
      <PkgDialogView name="a.pkg" state={{ info: { ...INFO, installed: true } }} icon={null} busy={false} onInstall={vi.fn()} onClose={vi.fn()} />,
    );
    expect(installed).toContain(t("cw_pkg_reinstall"));
    expect(installed).toContain("data-pkg-installed");
    const staged = renderToStaticMarkup(
      <PkgDialogView name="a.pkg" state={{ info: { ...INFO, staged: true } }} icon={null} busy={false} onInstall={vi.fn()} onClose={vi.fn()} />,
    );
    expect(staged).toContain(t("cw_pkg_staged_note"));
    const failed = renderToStaticMarkup(
      <PkgDialogView name="a.pkg" state={{ error: "nope" }} icon={null} busy={false} onInstall={vi.fn()} onClose={vi.fn()} />,
    );
    expect(failed).toContain("nope");
    expect(failed).not.toContain('data-action="pkg-install"');
  });
});
