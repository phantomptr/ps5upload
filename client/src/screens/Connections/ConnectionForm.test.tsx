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
  tidySmbAddress,
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

  it("splits a share typed with folders into share and start folder (Discord report)", () => {
    const t = tidySmbAddress({ ...emptyForm(), host: "nas", share: "XXX/YYY/ZZZ" });
    expect([t.host, t.share, t.startPath]).toEqual(["nas", "XXX", "/YYY/ZZZ"]);
    const lead = tidySmbAddress({ ...emptyForm(), host: "nas", share: "\\XXX\\YYY\\", startPath: "/ZZZ" });
    expect([lead.share, lead.startPath]).toEqual(["XXX", "/YYY/ZZZ"]);
  });

  it("splits a whole address pasted into Server or Share", () => {
    const unc = tidySmbAddress({ ...emptyForm(), host: "\\\\nas\\Media\\Games\\PS5" });
    expect([unc.host, unc.share, unc.startPath]).toEqual(["nas", "Media", "/Games/PS5"]);
    const url = tidySmbAddress({ ...emptyForm(), host: "smb://nas:4455/Media" });
    expect([url.host, url.port, url.share, url.startPath]).toEqual(["nas", 4455, "Media", ""]);
    const inShare = tidySmbAddress({ ...emptyForm(), host: "", share: "SMB://nas/Media/Games" });
    expect([inShare.host, inShare.share, inShare.startPath]).toEqual(["nas", "Media", "/Games"]);
    // A share already typed wins over the path in Server.
    const both = tidySmbAddress({ ...emptyForm(), host: "//nas/Other", share: "Media" });
    expect([both.host, both.share]).toEqual(["nas", "Media"]);
  });

  it("leaves a plain SMB form and other protocols alone", () => {
    const plain = { ...emptyForm(), host: "10.0.0.5", share: "games", startPath: "/ps5" };
    expect(tidySmbAddress(plain)).toBe(plain);
    const ipv6 = { ...emptyForm(), host: "fe80::1", share: "games" };
    expect(tidySmbAddress(ipv6)).toBe(ipv6);
    const ftp = { ...emptyForm(), protocol: "ftp" as const, host: "ftp://x/y", share: "a/b" };
    expect(tidySmbAddress(ftp)).toBe(ftp);
  });

  it("sends the tidied address and enables Test for a pasted UNC path", () => {
    const { connection } = toRequest({ ...emptyForm(), name: "NAS", host: "\\\\nas\\Media\\Games", share: "" });
    expect([connection.host, connection.share, connection.start_path]).toEqual(["nas", "Media", "/Games"]);
    const out = html({ host: "\\\\nas\\Media", share: "" });
    expect(disabledButton(out, "Test")).toBe(false);
  });

  it("explains a missing share in words, naming it", () => {
    const out = html(
      { share: "XXX" },
      {
        testResult: {
          ok: false,
          error: "No share named 'XXX' on this server (STATUS_BAD_NETWORK_NAME).",
          hint: "engine hint",
        },
      },
    );
    expect(out).toContain("No share named &#x27;XXX&#x27; on this server. The share is the first folder name only");
    expect(out).not.toContain("engine hint");
    const old = html({ share: "A" }, { testResult: { ok: false, error: "Protocol error: STATUS_BAD_NETWORK_NAME during TreeConnect" } });
    expect(old).toContain("No share named &#x27;A&#x27;");
  });
});
