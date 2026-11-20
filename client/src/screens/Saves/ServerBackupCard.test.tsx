import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";

// A static render reads the store's live state, not its initial one.
vi.mock("zustand", async (orig) => {
  const { liveZustand } = await import("../../test-utils/liveZustand");
  return liveZustand(await orig<typeof import("zustand")>());
});

import ServerBackupCard from "./ServerBackupCard";
import { ConsoleCopyChoice } from "./AutoBackupCard";
import { driveKeep, serverConsoleCopySupported, useServerBackupStore } from "../../state/serverSaveBackup";
import type { ServerBackupSettings } from "../../api/saveBackup";

const base: ServerBackupSettings = {
  mode: "after_close",
  target: "remote://nas/PS5 Game Saves",
  every_hours: 6,
  keep: 5,
  hosts: [],
  users: [],
};

describe("network-share backups: a copy on a PS5 drive (#885)", () => {
  afterEach(() => useServerBackupStore.setState({ settings: null, status: null, error: null }));

  it("is offered only by an engine that can make it, once a server folder is chosen", () => {
    expect(serverConsoleCopySupported({ console_drive: "" })).toBe(true);
    expect(serverConsoleCopySupported({})).toBe(false);
    expect(serverConsoleCopySupported(null)).toBe(false);
    useServerBackupStore.setState({ settings: { ...base, console_drive: "" } });
    const html = renderToStaticMarkup(<ServerBackupCard owners={[]} />);
    expect(html).toContain("server-save-backup-console");
    expect(html).toContain("Also keep a copy on a PS5 drive");
    // Its ids are its own: the automatic backups' card can show on the same page.
    expect(html).not.toContain("auto-save-backup-console");
    // An older engine: nothing that would snap back.
    useServerBackupStore.setState({ settings: base });
    expect(renderToStaticMarkup(<ServerBackupCard owners={[]} />)).not.toContain("server-save-backup-console");
    // No server folder yet: nothing to copy.
    useServerBackupStore.setState({ settings: { ...base, target: "", console_drive: "" } });
    expect(renderToStaticMarkup(<ServerBackupCard owners={[]} />)).not.toContain("server-save-backup-console");
  });

  it("keeps as many versions on the drive as on the server unless told otherwise", () => {
    expect(driveKeep({ keep: 5 })).toBe(5);
    expect(driveKeep({ keep: 5, console_keep: 0 })).toBe(5);
    expect(driveKeep({ keep: 5, console_keep: 1 })).toBe(1);
  });

  it("shows the drive's own versions to keep and what happens when the server is away", () => {
    const html = renderToStaticMarkup(
      <ConsoleCopyChoice
        testId="server-save-backup-console"
        drive="/mnt/ext1"
        drives={[{ path: "/mnt/ext1", kind: "ext", total_bytes: 1, free_bytes: 1 }]}
        connected
        loadError={null}
        status={{ at: 1_700_000_000_000, drive: "/mnt/ext1", copied: 3, skipped: 0, failed: 0, note: null }}
        onChange={() => {}}
      >
        <span data-testid="drive-keep-box" />
      </ConsoleCopyChoice>,
    );
    expect(html).toContain('data-testid="server-save-backup-console-drive"');
    expect(html).toContain('id="server-save-backup-console-select"');
    expect(html).toContain("drive-keep-box");
    expect(html).toContain("3 copied, 0 skipped, 0 failed.");
    // Off: the extras stay hidden.
    const off = renderToStaticMarkup(
      <ConsoleCopyChoice drive="" drives={[]} connected loadError={null} status={null} onChange={() => {}}>
        <span data-testid="drive-keep-box" />
      </ConsoleCopyChoice>,
    );
    expect(off).not.toContain("drive-keep-box");
  });

  it("renders the server card's drive keep box and note while the copy is on", () => {
    useServerBackupStore.setState({ settings: { ...base, console_drive: "/mnt/ext1", console_keep: 1 } });
    const html = renderToStaticMarkup(<ServerBackupCard owners={[]} />);
    expect(html).toContain("Versions to keep on the PS5 drive");
    expect(html).toMatch(/data-testid="server-save-backup-console-keep"[^>]*value="1"|value="1"[^>]*data-testid="server-save-backup-console-keep"/);
    expect(html).toContain("the PS5 drive still gets its copy");
  });
});
