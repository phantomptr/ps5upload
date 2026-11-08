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
const platform = vi.hoisted(() => ({ mobile: false }));
vi.mock("../lib/platform", async (orig) => ({
  ...(await orig<typeof import("../lib/platform")>()),
  isMobile: () => platform.mobile,
}));

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

  it("can be the screen's primary action", () => {
    const out = renderToStaticMarkup(<BrowseButton mode="file" remote primary onPick={noop} />);
    expect(out).toContain("bg-[var(--color-accent)]");
    const plain = renderToStaticMarkup(<BrowseButton mode="file" onPick={noop} />);
    expect(plain).not.toContain("bg-[var(--color-accent)]");
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

  it("calls the local entry This device on a phone (#534)", () => {
    platform.mobile = true;
    try {
      const out = renderToStaticMarkup(
        <BrowseMenu
          connections={[c("nas-1", "NAS", "smb")]}
          status={{}}
          onLocal={noop}
          onPickServer={noop}
          onAdd={noop}
        />,
      );
      expect(out).toContain("This device");
      expect(out).not.toContain("This computer");
      expect(out).toContain("NAS");
    } finally {
      platform.mobile = false;
    }
  });

  it("web UI: offers the server and this device, each naming the machine and the cost (#730)", () => {
    const out = renderToStaticMarkup(
      <BrowseMenu
        connections={[c("nas-1", "NAS", "smb")]}
        status={{}}
        onLocal={noop}
        onPickServer={noop}
        onAdd={noop}
        onThisDevice={noop}
        servers={false}
      />,
    );
    expect(out).toContain("On the server");
    expect(out).toContain("the computer the engine runs on");
    expect(out).toContain("This device");
    expect(out).toContain("the server needs free space");
    expect(out.indexOf("On the server")).toBeLessThan(out.indexOf("This device"));
    expect(out).not.toContain("This computer");
    // Folders read no saved server: none listed.
    expect(out).not.toContain("NAS");
    expect(out).not.toContain("Add a connection");
  });

  it("web UI: a folder button gets the source menu; the desktop button stays plain (#730)", () => {
    const web = renderToStaticMarkup(<BrowseButton mode="folder" thisDevice={noop} onPick={noop} />);
    expect(web).toContain('aria-haspopup="menu"');
    expect(web).toContain("Choose where to read from");
    const desktop = renderToStaticMarkup(<BrowseButton mode="folder" onPick={noop} />);
    expect(desktop).not.toContain("aria-haspopup");
    // The desktop's Add files menu is unchanged: This computer and the servers.
    const files = renderToStaticMarkup(
      <BrowseMenu connections={[c("nas-1", "NAS", "smb")]} status={{}} onLocal={noop} onPickServer={noop} onAdd={noop} />,
    );
    expect(files).toContain("This computer");
    expect(files).not.toContain("On the server");
    expect(files).toContain("NAS");
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
