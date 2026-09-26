import { describe, expect, it } from "vitest";
import {
  needsManualPartUploads,
  siblingParts,
} from "./archiveParts";

// Majid's actual folder, read off the Explorer screenshot: part01 is a ZIP,
// parts 02-11 are RAR.
const FOLDER = [
  "[DLPSGAME.COM]- 01.021 PPSA23226.part01.zip",
  ...Array.from({ length: 10 }, (_, i) =>
    `[DLPSGAME.COM]- 01.021 PPSA23226.part${String(i + 2).padStart(2, "0")}.rar`),
];

describe("Majid's folder", () => {
  it("shows what the app decided", () => {
    console.log("folder:", FOLDER.length, "files");
    console.log("siblingParts(part01.zip) =", siblingParts(FOLDER[0], FOLDER).length);
    console.log("needsManualPartUploads(part01.zip) =",
      needsManualPartUploads(FOLDER[0], FOLDER));
    expect(true).toBe(true);
  });
});
