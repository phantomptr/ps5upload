import { expect, it, vi } from "vitest";
import { planStreamSelection } from "./streamSelection";
it("reads the whole selection and orders base, update, DLC without duplicates", async () => {
  const read = vi.fn(async (path: string) => ({ head: { category: path.includes("base") ? "gd" : path.includes("dlc") ? "ac" : "gp", content_id: path } }));
  const result = await planStreamSelection(["/pc/update.pkg", "/pc/dlc.pkg", "/pc/base.pkg", "/pc/base.pkg"], read);
  expect(result.map((i) => i.path)).toEqual(["/pc/base.pkg", "/pc/update.pkg", "/pc/dlc.pkg"]);
  expect(result[0].contentId).toBe("/pc/base.pkg");
  expect(read).toHaveBeenCalledTimes(3);
});
it("does not produce a partial install plan when a header cannot be read", async () => {
  await expect(planStreamSelection(["/bad.pkg"], async () => { throw new Error("unreadable"); })).rejects.toThrow("unreadable");
});
