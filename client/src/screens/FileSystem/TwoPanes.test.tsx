import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", () => ({
  useTr: () => (key: string, a?: unknown, b?: string) => (typeof a === "string" ? a : b) ?? key,
}));

import { OtherPane, type OtherPaneState } from "./TwoPanes";

function pane(over: Partial<OtherPaneState>): OtherPaneState {
  const noop = () => {};
  return {
    place: { kind: "computer" },
    dir: "/Users/me",
    roots: [],
    entries: [
      { name: "Games", path: "/Users/me/Games", is_dir: true, size: 0 },
      { name: "notes.txt", path: "/Users/me/notes.txt", is_dir: false, size: 12 },
    ],
    more: null,
    loading: false,
    error: null,
    selected: new Set<string>(),
    revealed: null,
    clearReveal: noop,
    choose: noop,
    open: noop,
    up: noop,
    reload: async () => {},
    loadMore: noop,
    toggle: noop,
    setAll: noop,
    clearSelection: noop,
    ...over,
  } as OtherPaneState;
}

const row = (html: string, name: string) => {
  const at = html.indexOf(`data-other-row="${name}"`);
  return html.slice(html.lastIndexOf("<li", at), html.indexOf(">", at) + 1);
};

describe("OtherPane: going back highlights the folder come out of", () => {
  it("lights the folder row named by `revealed`, not ticked", () => {
    const html = renderToStaticMarkup(<OtherPane pane={pane({ revealed: "Games" })} servers={[]} consoles={[]} />);
    const games = row(html, "Games");
    expect(games).toContain("data-other-revealed");
    expect(games).toContain("fs-reveal");
    expect(games).not.toContain("data-ticked");
    expect(row(html, "notes.txt")).not.toContain("fs-reveal");
  });

  it("lights nothing without a folder to reveal, and never a file", () => {
    const none = renderToStaticMarkup(<OtherPane pane={pane({})} servers={[]} consoles={[]} />);
    expect(none).not.toContain("fs-reveal");
    const file = renderToStaticMarkup(<OtherPane pane={pane({ revealed: "notes.txt" })} servers={[]} consoles={[]} />);
    expect(file).not.toContain("fs-reveal");
  });
});
