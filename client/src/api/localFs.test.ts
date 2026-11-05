import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../lib/invokeLogged", () => ({ invoke: vi.fn() }));

import { invoke } from "../lib/invokeLogged";
import { localFs } from "./localFs";

const entries = (n: number) =>
  Array.from({ length: n }, (_, i) => ({ name: `f${i}`, path: `/d/f${i}`, is_dir: false, size: 1 }));

describe("localFs.listDirPage", () => {
  beforeEach(() => vi.mocked(invoke).mockReset());

  it("asks for a page and says where the next one starts", async () => {
    vi.mocked(invoke).mockResolvedValue(entries(2000));
    const page = await localFs.listDirPage("/d", 2000);
    expect(invoke).toHaveBeenCalledWith("local_list_dir", { path: "/d", offset: 2000, limit: 2000 });
    expect(page.next).toBe(4000);
    expect(page.entries).toHaveLength(2000);
  });

  it("a short page is the last one", async () => {
    vi.mocked(invoke).mockResolvedValue(entries(3));
    expect((await localFs.listDirPage("/d", 0)).next).toBeNull();
  });
});
