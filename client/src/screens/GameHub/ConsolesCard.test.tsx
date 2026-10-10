import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import type { GameView } from "../../api/games";
import { ConsolesCard } from "./ConsolesCard";

const offer = (category: string, version: string) => ({
  path: `/g/${category}.pkg`,
  name: `${category}.pkg`,
  version,
  content_id: "UP0000-PPSA01234_00-ASTRO0000000000",
  title: "",
  size_bytes: 1,
  category,
});

function render(view: GameView | null, titleId = "PPSA01234", running = false) {
  return renderToStaticMarkup(
    <MemoryRouter>
      <ConsolesCard
        titleId={titleId}
        consoles={[
          { host: "192.168.1.99", name: "Phat" },
          { host: "192.168.1.100", name: "Pro" },
        ]}
        connected="192.168.1.100"
        view={view}
        refresh={async (host) => ({ host, read_at: 0, installed: false, dlc_missing: [] })}
        onPlay={() => {}}
        launching={false}
        running={running}
        onStop={() => {}}
        sendHost={null}
        setSendHost={() => {}}
      />
    </MemoryRouter>,
  );
}

describe("the game page's console rows", () => {
  const nowSec = Math.floor(Date.now() / 1000);

  it("shows each console's state and how old it is, the connected one first", () => {
    const html = render({
      title_id: "PPSA01234",
      title: "Astro",
      platform: "PS5",
      cover: null,
      copies: [],
      consoles: [
        { host: "192.168.1.100", read_at: nowSec, installed: true, version: "01.004", dlc_missing: [] },
        {
          host: "192.168.1.99",
          read_at: nowSec - 2 * 3600,
          installed: true,
          version: "01.002",
          update: offer("gp", "01.004"),
          dlc_missing: [],
        },
      ],
    });
    expect(html.indexOf("Pro")).toBeLessThan(html.indexOf("Phat"));
    expect(html).toContain("Installed · v01.004");
    expect(html).toContain("now");
    expect(html).toContain("as of 2 h ago");
    expect(html).toContain("Install update 01.004");
    // Play only on the connected console.
    expect(html.match(/Play</g)?.length).toBe(1);
  });

  it("offers Close game instead of Play on the console the game is running on", () => {
    const html = render(
      {
        title_id: "PPSA01234",
        title: "Astro",
        platform: "PS5",
        cover: null,
        copies: [],
        consoles: [
          { host: "192.168.1.100", read_at: nowSec, installed: true, dlc_missing: [] },
          { host: "192.168.1.99", read_at: nowSec, installed: true, dlc_missing: [] },
        ],
      },
      "PPSA01234",
      true,
    );
    expect(html.match(/Close game</g)?.length).toBe(1);
    expect(html).not.toContain("Play<");
  });

  it("says a console has not been checked when it never read the game", () => {
    const html = render({ title_id: "PPSA01234", title: "Astro", platform: "PS5", cover: null, copies: [], consoles: [] });
    expect(html.match(/Not checked yet/g)?.length).toBe(2);
  });

  it("explains that a game without a title ID cannot be checked on a console", () => {
    expect(render(null, "MY-FOLDER")).toContain("no title ID");
  });
});
