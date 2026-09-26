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

import type { Connection } from "../../api/remote";
import { ServersCardView } from "./ServersCard";

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

describe("Servers on Home", () => {
  it("invites the first connection when there is none", () => {
    const out = renderToStaticMarkup(
      <ServersCardView connections={[]} status={{}} onBrowse={noop} onAdd={noop} />,
    );
    expect(out).toContain("Connect a NAS or server");
    expect((out.match(/>Browse</g) ?? []).length).toBe(0);
  });

  it("lists each server with a Browse, and offers another", () => {
    const out = renderToStaticMarkup(
      <ServersCardView
        connections={[c("nas-1", "NAS", "smb"), c("box-2", "Seedbox", "sftp")]}
        status={{ "nas-1": "reachable" }}
        onBrowse={noop}
        onAdd={noop}
      />,
    );
    expect(out).toContain("NAS");
    expect(out).toContain("SMB");
    expect(out).toContain("Seedbox");
    expect(out).toContain("SFTP");
    expect((out.match(/>Browse</g) ?? []).length).toBe(2);
    expect(out).toContain("Add a server");
  });
});
