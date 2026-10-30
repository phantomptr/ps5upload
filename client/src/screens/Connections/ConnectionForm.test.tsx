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

import {
  ConnectionFormView,
  emptyForm,
  switchProtocol,
  toRequest,
  type ConnectionFormProps,
  type FormValue,
} from "./ConnectionForm";

const noop = () => {};

function html(value: Partial<FormValue>, p: Partial<ConnectionFormProps> = {}) {
  return renderToStaticMarkup(
    <ConnectionFormView
      value={{ ...emptyForm(), name: "NAS", host: "10.0.0.5", share: "games", ...value }}
      onChange={noop}
      shares={[]}
      testResult={null}
      busy={false}
      hasSavedSecret={false}
      onTest={noop}
      onSave={noop}
      onCancel={noop}
      onAcceptHostKey={noop}
      onPickKeyFile={noop}
      onListShares={noop}
      {...p}
    />,
  );
}

/** Whether the <button> labelled `label` carries the disabled attribute. */
function disabledButton(out: string, label: string): boolean {
  const re = new RegExp(`<button[^>]*>(?:(?!</button>).)*${label}(?:(?!</button>).)*</button>`, "s");
  const tag = out.match(re)?.[0] ?? "";
  expect(tag, `no ${label} button`).not.toBe("");
  return /<button[^>]*\sdisabled=""/.test(tag);
}

describe("connection form", () => {
  it("browses the whole server for a new SFTP/FTP connection, and sends it only for those", () => {
    const sftp = { ...emptyForm(), protocol: "sftp" as const, port: 22, name: "n", host: "h", startPath: " /mnt/games " };
    expect(toRequest(sftp).connection).toMatchObject({ from_root: true, start_path: "/mnt/games" });
    expect(toRequest({ ...sftp, protocol: "ftp" }).connection.from_root).toBe(true);
    expect(toRequest({ ...sftp, fromRoot: false }).connection.from_root).toBe(false);
    expect(toRequest({ ...sftp, protocol: "smb" }).connection.from_root).toBe(false);
    expect(toRequest({ ...sftp, protocol: "webdav" }).connection.from_root).toBe(false);
    expect(html({ protocol: "sftp", port: 22 })).toContain("Browse the whole server");
    expect(html({ protocol: "smb", port: 445 })).not.toContain("Browse the whole server");
  });

  it("warns that saved links need adding again when an old connection switches", () => {
    const relink = "add them again after this change";
    expect(html({ protocol: "sftp", port: 22, fromRoot: true }, { savedFromRoot: false })).toContain(relink);
    expect(html({ protocol: "sftp", port: 22, fromRoot: false }, { savedFromRoot: false })).not.toContain(relink);
    expect(html({ protocol: "sftp", port: 22, fromRoot: true }, { savedFromRoot: true })).not.toContain(relink);
    expect(html({ protocol: "sftp", port: 22, fromRoot: true }, { savedFromRoot: null })).not.toContain(relink);
  });

  it("keeps a custom port when switching protocol", () => {
    const smb = { ...emptyForm(), protocol: "smb" as const, port: 445 };
    expect(switchProtocol(smb, "ftp").port).toBe(21);
    expect(switchProtocol(smb, "sftp").port).toBe(22);
    expect(switchProtocol({ ...smb, port: 4450 }, "ftp").port).toBe(4450);
  });

  it("labels plain FTP as unencrypted", () => {
    expect(html({ protocol: "ftp", port: 21 })).toContain("Unencrypted");
    expect(html({ protocol: "ftps", port: 21 })).not.toContain("Unencrypted");
  });

  it("offers WebDAV with its ports, its root hint and the self-signed switch", () => {
    const smb = { ...emptyForm(), protocol: "smb" as const, port: 445 };
    expect(switchProtocol(smb, "webdav").port).toBe(80);
    expect(switchProtocol(smb, "webdavs").port).toBe(443);
    expect(html({ protocol: "webdav", port: 80 })).toContain("Unencrypted");
    expect(html({ protocol: "webdav", port: 80 })).toContain("remote.php/dav");
    expect(html({ protocol: "webdavs", port: 443 })).toContain("self-signed");
    expect(html({ protocol: "webdav", port: 80 })).not.toContain("self-signed");
    const req = toRequest({ ...emptyForm(), protocol: "webdavs", port: 443, insecureTls: true, name: "n", host: "h" });
    expect(req.connection.insecure_tls).toBe(true);
    expect(toRequest({ ...emptyForm(), protocol: "webdav", insecureTls: true, name: "n", host: "h" }).connection.insecure_tls).toBe(false);
  });

  it("asks for the share only on SMB and hides credentials for guests", () => {
    expect(html({ protocol: "smb" })).toContain("Share");
    expect(html({ protocol: "sftp", port: 22 })).not.toContain(">Share<");
    const guest = html({ protocol: "smb", guest: true });
    expect(guest).not.toContain("Password");
    expect(html({ protocol: "smb", guest: false })).toContain("Password");
  });

  it("offers a key file for SFTP", () => {
    expect(html({ protocol: "sftp", port: 22, guest: false, authKind: "key" })).toContain("Key file");
  });

  it("enables Save only after a passing test", () => {
    expect(disabledButton(html({}, { testResult: null }), "Save")).toBe(true);
    expect(disabledButton(html({}, { testResult: { ok: false, error: "nope" } }), "Save")).toBe(true);
    expect(disabledButton(html({}, { testResult: { ok: true } }), "Save")).toBe(false);
  });

  it("shows why a test failed, with the hint", () => {
    const out = html({}, { testResult: { ok: false, error: "Sign-in failed: x", hint: "Check the password." } });
    expect(out).toContain("Sign-in failed");
    expect(out).toContain("Check the password.");
  });

  it("asks to accept a new SFTP host key", () => {
    const out = html(
      { protocol: "sftp", port: 22 },
      { testResult: { ok: false, error: "Sign-in failed: unknown host key", host_key: "SHA256:abc" } },
    );
    expect(out).toContain("SHA256:abc");
    expect(out).toContain("Accept");
  });
});
