import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", () => ({
  useTr:
    () =>
    (
      key: string,
      vars?: Record<string, string | number>,
      fallback?: string,
    ) => {
      let s = fallback ?? key;
      for (const [k, v] of Object.entries(vars ?? {}))
        s = s.replace(`{${k}}`, String(v));
      return s;
    },
}));

import {
  NP_FAKE_SIGNIN_URL,
  NP_FAKE_SIGNIN_VERSION,
  NpSignInCard,
} from "./NpSignInCard";

describe("the np-fake-signin guidance on Remote Play", () => {
  it("points at the v1.4 PS5 build", () => {
    expect(NP_FAKE_SIGNIN_VERSION).toBe("v1.4");
    expect(NP_FAKE_SIGNIN_URL).toBe(
      "https://git.etawen.dev/earthonion/np-fake-signin/releases/download/v1.4/np-fake-signin-ps5.elf",
    );
  });

  it("says what to download, what it unlocks, and how to undo it", () => {
    const html = renderToStaticMarkup(
      <NpSignInCard onDownload={() => {}} onOpenPayloads={() => {}} />,
    );
    expect(html).toContain("np-fake-signin-ps5.elf");
    expect(html).toContain("v1.4");
    // What the user gets: the console's own Remote Play switch.
    expect(html).toContain("System → Remote Play");
    // It needs an activated account, and a restart to take effect.
    expect(html).toContain("activated");
    expect(html).toContain("Restart the PS5");
    expect(html).toContain("Sign out");
    expect(html).toContain("np-signin-download");
    expect(html).toContain("np-signin-open-payloads");
  });
});
