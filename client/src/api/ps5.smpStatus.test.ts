import { describe, expect, it } from "vitest";
import { normalizeSmpStatus } from "./ps5";

describe("normalizeSmpStatus", () => {
  it("fills the lists a partial answer leaves out, so the game page never crashes on them", () => {
    const s = normalizeSmpStatus({ running: true });
    expect(s.errors).toEqual([]);
    expect(s.mounted_images).toEqual([]);
    expect(s.running).toBe(true);
    expect(s.installed).toBe(false);
    expect(() => s.errors.some((e) => e.startsWith("proc list:"))).not.toThrow();
  });

  it("treats no answer at all as nothing running", () => {
    const s = normalizeSmpStatus(null);
    expect(s).toMatchObject({ running: false, installed: false, errors: [], mounted_images: [] });
  });

  it("keeps a full answer as it came, and fills Kraken Mount's lists when they are missing", () => {
    const s = normalizeSmpStatus({
      installed: true,
      running: false,
      errors: ["proc list: denied"],
      mounted_images: [],
      mounter: "kraken",
      kraken: { installed: true, running: true } as never,
    });
    expect(s.errors).toEqual(["proc list: denied"]);
    expect(s.mounter).toBe("kraken");
    expect(s.kraken?.games).toEqual([]);
    expect(s.kraken?.title_ids).toEqual([]);
  });
});
