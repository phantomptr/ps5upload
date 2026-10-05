import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../lib/invokeLogged", () => ({ invoke: vi.fn() }));

import { invoke } from "../lib/invokeLogged";
import { linkProbe, pkgConsoleProbe, rarPackages, startLinkDownload } from "./links";

const inv = vi.mocked(invoke);

beforeEach(() => vi.clearAllMocks());

describe("link + rar package api", () => {
  it("probes a link with the tls choice", async () => {
    inv.mockResolvedValue({ kind: "pkg", filename: "a", total_size: 1, ranges: true, content_type: "" });
    const r = await linkProbe("https://h/x", true);
    expect(r.kind).toBe("pkg");
    expect(inv).toHaveBeenCalledWith("link_probe", { url: "https://h/x", insecure_tls: true });
  });

  it("starts a download to a console folder and returns the job id", async () => {
    inv.mockResolvedValue({ job_id: "j1" });
    const id = await startLinkDownload({
      url: "https://h/x.elf",
      destDir: "/data/dl",
      addr: "10.0.0.5",
    });
    expect(id).toBe("j1");
    expect(inv).toHaveBeenCalledWith("link_download", {
      req: {
        url: "https://h/x.elf",
        dest_dir: "/data/dl",
        addr: "10.0.0.5",
        file_name: null,
        insecure_tls: false,
      },
    });
  });

  it("lists a rar's packages and probes a console package", async () => {
    inv.mockResolvedValueOnce({ packages: [{ path: "a/b.pkg", size: 3 }] });
    expect(await rarPackages("/x.rar", "pw")).toEqual([{ path: "a/b.pkg", size: 3 }]);
    expect(inv).toHaveBeenLastCalledWith("rar_packages", {
      req: { archive_path: "/x.rar", password: "pw" },
    });
    inv.mockResolvedValueOnce({ category: "gp" });
    await pkgConsoleProbe("10.0.0.5", "/data/a.pkg");
    expect(inv).toHaveBeenLastCalledWith("pkg_console_probe", {
      host: "10.0.0.5",
      path: "/data/a.pkg",
    });
  });
});
