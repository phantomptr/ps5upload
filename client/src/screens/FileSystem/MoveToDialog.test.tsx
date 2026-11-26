import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", () => ({
  useTr: () =>
    (key: string, vars?: Record<string, string | number>, fallback?: string) => {
      let s = fallback ?? key;
      for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
      return s;
    },
}));

import type { Volume } from "../../api/ps5";
import { MoveToDialog } from "./MoveToDialog";

const data: Volume = {
  path: "/data",
  mount_from: "/dev/x",
  fs_type: "ufs",
  total_bytes: 1000,
  free_bytes: 400,
  writable: true,
} as Volume;

const base = {
  open: true,
  host: "192.0.2.5",
  addr: "192.0.2.5:9113",
  items: [{ path: "/data/game", name: "game", size: 10 }],
  volumes: [data],
  startPath: "/data",
  onCancel: () => undefined,
  onConfirm: () => undefined,
};

// No DOM here (vitest in node): render to markup and check what a user would see.
describe("Move to…: saved servers (#953)", () => {
  it("lists each saved server with Copy and Move, and says a move removes the originals only after a check", () => {
    const html = renderToStaticMarkup(
      <MoveToDialog
        {...base}
        servers={[
          { id: "a1", name: "NAS" },
          { id: "b2", name: "Backup FTP" },
        ]}
        onServer={() => undefined}
      />,
    );
    expect(html).toContain('data-testid="move-servers"');
    expect(html).toContain("Saved servers");
    expect(html).toContain("NAS");
    expect(html).toContain("Backup FTP");
    expect(html.split("Move there…").length - 1).toBe(2);
    expect(html.split("Copy there…").length - 1).toBe(2);
    expect(html).toContain("checks the copy, then removes the originals from the PS5");
  });

  it("shows no server section without servers or without a handler", () => {
    expect(renderToStaticMarkup(<MoveToDialog {...base} onServer={() => undefined} />)).not.toContain("move-servers");
    expect(renderToStaticMarkup(<MoveToDialog {...base} servers={[{ id: "a1", name: "NAS" }]} />)).not.toContain(
      "move-servers",
    );
  });
});
