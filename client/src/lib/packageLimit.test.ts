import { describe, expect, it } from "vitest";

import { GIB, gib, limitFor, packageFit, parseFirmware, withContainer } from "./packageLimit";

describe("packageLimit", () => {
  it("reads firmware however it is written", () => {
    expect(parseFirmware("13.60")).toEqual({ major: 13, minor: 60 });
    expect(parseFirmware("05.10")).toEqual({ major: 5, minor: 10 });
    expect(parseFirmware("9.6")).toEqual({ major: 9, minor: 60 });
    expect(parseFirmware("0x1200000000000000")).toEqual({ major: 12, minor: 0 });
    expect(parseFirmware("nope")).toBeNull();
    expect(parseFirmware(null)).toBeNull();
  });

  it("has each firmware line's own ceiling, and only 13.x counts the add-on space", () => {
    const sizes = { ac_gib: 64, app_gib: 320 };
    expect(limitFor({ major: 5, minor: 10 }, sizes)).toBeNull();
    expect(limitFor({ major: 6, minor: 50 }, sizes)?.maxPackage).toBe(260_436_918_272);
    expect(limitFor({ major: 10, minor: 60 }, sizes)?.maxPackage).toBe(256 * GIB);
    expect(limitFor({ major: 12, minor: 0 }, sizes)?.maxPackage).toBe(320 * GIB);
    expect(limitFor({ major: 12, minor: 0 }, { app_gib: 400 })?.maxPackage).toBe(256 * GIB);
    expect(limitFor({ major: 11, minor: 0 }, null)?.maxPackage).toBe(256 * GIB);
    const fw13 = limitFor({ major: 13, minor: 60 }, sizes);
    expect(fw13).toEqual({ band: "fw1300", cap: 450 * GIB, reserved: 64 * GIB, maxPackage: 386 * GIB });
    expect(limitFor({ major: 13, minor: 0 }, {})?.maxPackage).toBe(450 * GIB);
  });

  it("checks against the target, then the console, then the game, then 13.x", () => {
    const sizes = { ac_gib: 64 };
    const size = 300 * GIB;
    expect(packageFit({ size, estimated: false, target: "9.60", console: "13.60", sizes })).toMatchObject({
      basis: "target",
      fits: false,
    });
    expect(packageFit({ size, estimated: false, console: "13.60", sizes })).toMatchObject({ basis: "console", fits: true });
    expect(packageFit({ size, estimated: false, required: "13.20", sizes })).toMatchObject({
      basis: "game",
      fits: true,
      olderMiss: false,
    });
    expect(packageFit({ size, estimated: false, required: "07.00", sizes })).toMatchObject({
      basis: "assumed",
      fits: true,
      olderMiss: true,
      fw550Miss: false,
    });
    // Past 5.50 to 6.xx's 242.55 GiB but within 256 GiB: only that line misses it.
    expect(packageFit({ size: 250 * GIB, estimated: false, sizes })).toMatchObject({
      olderMiss: false,
      fw550Miss: true,
    });
    expect(packageFit({ size: 242 * GIB, estimated: false, sizes }).fw550Miss).toBe(false);
    expect(packageFit({ size: 256 * GIB, estimated: false, sizes }).fw550Miss).toBe(true);
    expect(packageFit({ size: 250 * GIB, estimated: false, console: "6.50", sizes }).fw550Miss).toBe(false);
    // The add-on space is what puts 400 GiB over on 13.x.
    expect(packageFit({ size: 400 * GIB, estimated: false, console: "13.60", sizes }).fits).toBe(false);
    expect(packageFit({ size: 400 * GIB, estimated: false, console: "13.60", sizes: {} }).fits).toBe(true);
  });

  it("estimates the layout index from the package's size", () => {
    const fit = packageFit({ size: 65536 * 10, estimated: false, layoutIndexFixed: 1000 });
    expect(fit.layoutIndex).toBe(1000 + 80);
    expect(packageFit({ size: 1, estimated: false }).layoutIndex).toBeNull();
  });

  it("shows sizes the way the line does", () => {
    expect(gib(580 * GIB)).toBe("580");
    expect(gib(216.4 * GIB)).toBe("216.4");
    expect(withContainer(65536)).toBe(65536 + 72);
  });
});
