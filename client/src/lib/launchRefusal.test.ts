import { describe, expect, it, vi } from "vitest";

import { handleHomebrewRefusal, isHomebrewRefusal } from "./launchRefusal";

const deps = (
  procs: Array<{ name: string }> | Error,
  relaunchFails = false,
) => ({
  processes: vi.fn(async () => {
    if (procs instanceof Error) throw procs;
    return procs;
  }),
  chmod777: vi.fn(async () => {}),
  relaunch: vi.fn(async () => {
    if (relaunchFails) throw new Error("launch_sony_error_0x80940033");
  }),
});

describe("a launch the PS5 refused with 0x80940033", () => {
  it("is recognised from the helper's reason", () => {
    expect(isHomebrewRefusal("launch_sony_error_0x80940033")).toBe(true);
    expect(isHomebrewRefusal("launch_sony_error_0x8094000F")).toBe(false);
  });

  it("names kstuff when it is not running, and touches nothing", async () => {
    const d = deps([{ name: "SceShellUI" }]);
    expect(await handleHomebrewRefusal("/data/homebrew/G", d)).toEqual({
      kind: "no_kstuff",
    });
    expect(d.chmod777).not.toHaveBeenCalled();
    expect(d.relaunch).not.toHaveBeenCalled();
  });

  it("with kstuff running, makes the folder 0777 and launches again", async () => {
    const d = deps([{ name: "kstuff.elf" }]);
    expect(await handleHomebrewRefusal("/data/homebrew/G", d)).toEqual({
      kind: "fixed",
    });
    expect(d.chmod777).toHaveBeenCalledWith("/data/homebrew/G");
    expect(d.relaunch).toHaveBeenCalledTimes(1);
  });

  it("still repairs the folder when the process list cannot be read", async () => {
    const d = deps(new Error("busy"));
    expect(await handleHomebrewRefusal("/data/homebrew/G", d)).toEqual({
      kind: "fixed",
    });
  });

  it("reports a second refusal, and a title with no folder, as refused", async () => {
    expect(
      await handleHomebrewRefusal(
        "/data/homebrew/G",
        deps([{ name: "kstuff" }], true),
      ),
    ).toEqual({
      kind: "refused",
      shadowmount: false,
    });
    expect(await handleHomebrewRefusal("", deps([{ name: "kstuff" }]))).toEqual(
      {
        kind: "refused",
        shadowmount: false,
      },
    );
  });
});

describe("a refusal with kstuff running that the folder repair did not fix", () => {
  it("says ShadowMount+ is running, so the message can point at it", async () => {
    const d = deps(
      [{ name: "kstuff.elf" }, { name: "shadowmountplus.elf" }],
      true,
    );
    expect(await handleHomebrewRefusal("/data/homebrew/G", d)).toEqual({
      kind: "refused",
      shadowmount: true,
    });
  });

  it("says so for a game with no folder to repair too (an image ShadowMount+ mounts)", async () => {
    const d = deps([{ name: "kstuff.elf" }, { name: "shadowmountplus.elf" }]);
    expect(await handleHomebrewRefusal("", d)).toEqual({
      kind: "refused",
      shadowmount: true,
    });
    expect(d.relaunch).not.toHaveBeenCalled();
  });

  it("does not blame ShadowMount+ when it is not running", async () => {
    const d = deps([{ name: "kstuff.elf" }], true);
    expect(await handleHomebrewRefusal("/data/homebrew/G", d)).toEqual({
      kind: "refused",
      shadowmount: false,
    });
  });
});
