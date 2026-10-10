import { describe, expect, it } from "vitest";

import { fileSystemLinkFor } from "./searchLink";

const open = (link: string) => decodeURIComponent(link.replace("/files?path=", ""));

describe("fileSystemLinkFor", () => {
  it("opens a folder hit at the folder itself", () => {
    expect(open(fileSystemLinkFor({ path: "/mnt/ext0/games/PPSA01234", kind: "dir" }))).toBe(
      "/mnt/ext0/games/PPSA01234",
    );
  });

  it("opens a file hit at the folder that holds it", () => {
    expect(open(fileSystemLinkFor({ path: "/data/pkgs/Game Name.pkg", kind: "file" }))).toBe(
      "/data/pkgs",
    );
  });

  it("opens a file at the root on /", () => {
    expect(open(fileSystemLinkFor({ path: "/eboot.bin", kind: "file" }))).toBe("/");
  });

  it("encodes the path for the query string", () => {
    expect(fileSystemLinkFor({ path: "/data/a b&c", kind: "dir" })).toBe(
      "/files?path=%2Fdata%2Fa%20b%26c",
    );
  });
});
