import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { PairingPanel } from "./PairingDialog";

const noop = () => {};
const panel = (over: Partial<Parameters<typeof PairingPanel>[0]>) =>
  renderToStaticMarkup(
    <PairingPanel
      view={null}
      busy={false}
      error={null}
      onConfirm={noop}
      onRetry={noop}
      onCancel={noop}
      {...over}
    />,
  );

describe("pairing_dialog_takes_the_code_from_the_console_screen", () => {
  it("asks for the six digits the console shows and never displays a code", () => {
    const html = panel({
      view: { state: "code", consoleName: "PS5-Pro" },
    });
    expect(html).toContain("Enter the code shown on your PS5");
    expect(html).toContain("PS5-Pro is asking to pair");
    expect(html).toContain('data-testid="pairing-code-input"');
    expect(html).toContain('maxLength="6"');
    expect(html).toContain('inputMode="numeric"');
    expect(html).not.toContain("Codes match");
    expect(html).not.toContain("didn&#x27;t match");
  });

  it("says a wrong code did not match and lets the user try again", () => {
    const html = panel({
      view: { state: "wrong_code", consoleName: "PS5-Pro" },
    });
    expect(html).toContain("That code didn&#x27;t match");
    expect(html).toContain('data-testid="pairing-code-input"');
  });

  it("explains a different console at the pinned address and offers to forget the old one", () => {
    const html = panel({ view: { state: "wrong_console" } });
    expect(html).toContain("A different PS5 answered at this address");
    expect(html).toContain("Forget the old one and pair this one");
    expect(html).not.toContain("pairing-code-input");
  });

  it("explains how to reopen a closed pairing window and offers a retry", () => {
    const html = panel({ view: { state: "closed" } });
    expect(html).toContain("The PS5 is not accepting new pairings");
    expect(html).toContain("already paired");
    expect(html).toContain("restart the helper");
    expect(html).toContain("Try again");
    expect(html).not.toContain("pairing-code-input");
  });

  it("offers to send the helper again when the window is closed: a helper the app sends pairs by itself", () => {
    const html = panel({ view: { state: "closed" }, onResend: () => {} });
    expect(html).toContain("Resend helper");
    expect(html).toContain("pairing-resend-helper");
  });

  it("has no resend button where the app cannot send the helper", () => {
    const html = panel({ view: { state: "closed" } });
    expect(html).not.toContain("pairing-resend-helper");
  });

  it("says when the console could not be reached", () => {
    const html = panel({ view: null, error: "timed out" });
    expect(html).toContain("Pairing failed: timed out");
    expect(html).toContain("Try again");
  });

  it("shows progress while the first answer is pending", () => {
    expect(panel({ view: null })).toContain("Contacting the PS5");
  });
});
