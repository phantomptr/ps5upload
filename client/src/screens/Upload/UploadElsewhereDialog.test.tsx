import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { UploadElsewhereDialog } from "./UploadElsewhereDialog";

const render = (dest: string, flat?: boolean) =>
  renderToStaticMarkup(
    <UploadElsewhereDialog
      open
      addr="10.0.0.2"
      dest={dest}
      flat={flat}
      onCancel={() => undefined}
      onPick={() => undefined}
    />,
  );

describe("Upload somewhere else", () => {
  it("starts in the destination's folder and keeps the name", () => {
    // Browsing starts in /data/homebrew, which is where it goes now.
    expect(render("/data/homebrew/Game")).toContain("That&#x27;s where it goes now");
  });

  it("an archive extracted straight into a folder: that folder is the destination", () => {
    // Starts in the folder itself, the place it goes now (not its parent).
    const flat = render("/data/homebrew/Game", true);
    expect(flat).toContain('title="/data/homebrew/Game"');
    expect(flat).toContain("That&#x27;s where it goes now");
    expect(render("/data/homebrew/Game")).toContain('title="/data/homebrew"');
  });
});
