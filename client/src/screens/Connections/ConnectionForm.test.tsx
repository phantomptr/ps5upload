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
