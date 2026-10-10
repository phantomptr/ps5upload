// @ts-expect-error -- the app tsconfig has no node types; Vitest runs this under Node.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const read = (name: string): string =>
  readFileSync(new URL(name, import.meta.url).pathname, "utf8");

// plugin-dialog's confirm throws in the browser build, which turned every
// Profile confirm (avatar, delete user, account id) into an error there.
describe("Profile in the browser build", () => {
  it("confirms through the app's own dialog", () => {
    const src = read("./index.tsx");
    expect(src).not.toMatch(/@tauri-apps\/plugin-dialog/);
    expect(src).toMatch(/useConfirm\(\)/);
  });

  it("only offers Payloads where Payloads can open", () => {
    const src = read("./NpSignInSection.tsx");
    expect(src).toMatch(/\{desktop && \(\s*<Button[\s\S]{0,120}navigate\("\/payloads"\)/);
  });
});
