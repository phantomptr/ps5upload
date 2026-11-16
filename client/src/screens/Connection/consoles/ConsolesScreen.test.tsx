import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { useRosterStore } from "../../../state/roster";
import ConsolesScreen, { openCardLink } from "./ConsolesScreen";

const PRO = { id: "pro", name: "Pro", host: "192.168.1.100" };
const PHAT = { id: "phat", name: "Phat", host: "192.168.1.101" };

function page(url = "/connection") {
  return renderToStaticMarkup(
    <MemoryRouter initialEntries={[url]}>
      <ConsolesScreen />
    </MemoryRouter>,
  );
}

const cardsIn = (html: string) => [...html.matchAll(/data-console="([^"]+)"/g)].map((m) => m[1]);

/** A server render reads the store's initial state: the roster is set there. */
function roster(patch: { profiles?: (typeof PRO)[]; active_id: string | null }) {
  Object.assign(useRosterStore.getInitialState(), patch);
}

describe("Consoles page › the console picked at the top", () => {
  const init = useRosterStore.getInitialState();
  const was = { profiles: init.profiles, active_id: init.active_id };
  afterEach(() => Object.assign(init, was));

  it("shows only the selected console's card, and picking another console shows its card", () => {
    roster({ profiles: [PRO, PHAT], active_id: "pro" });
    let html = page();
    expect(cardsIn(html)).toEqual(["pro"]);
    expect(html).toContain(">Pro</h2>");
    expect(html).not.toContain(">Phat</h2>");

    roster({ active_id: "phat" });
    html = page();
    expect(cardsIn(html)).toEqual(["phat"]);
    expect(html).toContain(">Phat</h2>");
    expect(html).not.toContain(">Pro</h2>");
  });

  it("a link to one console's card picks that console first, then drops its parameters", () => {
    const ids = ["pro", "phat"];
    const link = openCardLink(new URLSearchParams("console=phat&devices=1"), "pro", ids);
    expect(link?.pick).toBe("phat");
    // The device list stays asked for until the picked console's card has read it.
    expect(link?.next.toString()).toBe("devices=1");
    const after = openCardLink(new URLSearchParams("devices=1"), "phat", ids);
    expect(after?.pick).toBeNull();
    expect(after?.next.toString()).toBe("");
    // Already picked, or not on the list: nothing to pick, the parameter just goes.
    expect(openCardLink(new URLSearchParams("console=pro"), "pro", ids)).toMatchObject({ pick: null });
    expect(openCardLink(new URLSearchParams("console=gone&tab=x"), "pro", ids)?.next.toString()).toBe("tab=x");
    expect(openCardLink(new URLSearchParams("add=1"), "pro", ids)).toBeNull();
  });

  it("with the link's devices=1, the picked console's card opens its device list", () => {
    roster({ profiles: [PRO, PHAT], active_id: "phat" });
    const html = page("/connection?devices=1");
    expect(html).toContain('aria-expanded="true"');
  });

  it("with no console saved: the empty state, and no card", () => {
    roster({ profiles: [], active_id: null });
    const html = page();
    expect(html).toContain('data-testid="consoles-empty"');
    expect(cardsIn(html)).toEqual([]);
  });
});
