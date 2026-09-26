import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../state/lang", () => ({
  useTr: () =>
    (key: string, vars?: Record<string, string | number>, fallback?: string) => {
      let s = fallback ?? key;
      for (const [k, v] of Object.entries(vars ?? {})) s = s.replace(`{${k}}`, String(v));
      return s;
    },
}));
vi.mock("react-router", () => ({ useNavigate: () => () => {} }));

import type { Connection } from "../api/remote";
import { BrowseButton, BrowseMenu, PathLabelView } from "./BrowseButton";

const noop = () => {};
const c = (id: string, name: string, protocol: Connection["protocol"]): Connection => ({
  id,
  name,
  protocol,
  host: "10.0.0.5",
  port: 445,
  share: "",
  user: "",
  start_path: "",
  host_key: null,
  has_secret: false,
});

describe("BrowseButton", () => {
  it("is a plain button where a screen takes only local picks", () => {
    const out = renderToStaticMarkup(<BrowseButton mode="file" onPick={noop} />);
    expect(out).toContain("Browse");
    expect(out).not.toContain("aria-haspopup");
  });

  it("has a server menu where remote picks are accepted", () => {
    const out = renderToStaticMarkup(<BrowseButton mode="file" remote onPick={noop} />);
    expect(out).toContain('aria-haspopup="menu"');
  });

  it("lists servers with their protocol, greys unreachable ones, and offers Add", () => {
    const out = renderToStaticMarkup(
      <BrowseMenu
        connections={[c("nas-1", "NAS", "smb"), c("box-2", "Seedbox", "sftp")]}
        status={{ "box-2": "offline" }}
        onLocal={noop}
        onPickServer={noop}
        onAdd={noop}
      />,
    );
    expect(out).toContain("This computer");
    expect(out).toContain("NAS");
    expect(out).toContain("(SMB)");
    expect(out).toContain("Seedbox");
    expect(out).toContain("(SFTP)");
    expect(out).toContain("not reachable");
    expect(out).toContain("Add a connection");
  });

  it("shows a remote path by its server's name, and a local path as is", () => {
    const nameOf = (id: string) => (id === "nas-1" ? "NAS" : undefined);
    expect(
      renderToStaticMarkup(<PathLabelView path="remote://nas-1/games/a.pkg" nameOf={nameOf} />),
    ).toContain("NAS › games/a.pkg");
    expect(
      renderToStaticMarkup(<PathLabelView path="/Users/me/a.pkg" nameOf={nameOf} />),
    ).toContain("/Users/me/a.pkg");
  });
});
