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

import { NP_FAKE_SIGNIN_VERSION, NpSignInCard } from "./NpSignInCard";

describe("the np-fake-signin guidance on Remote Play", () => {
  it("names the v1.4 build", () => {
    expect(NP_FAKE_SIGNIN_VERSION).toBe("v1.4");
  });

  it("says what it unlocks and how to undo it, and sends the user to Profile", () => {
    const html = renderToStaticMarkup(<NpSignInCard onOpenProfile={() => {}} />);
    expect(html).toContain("v1.4");
    // What the user gets: the console's own Remote Play switch.
    expect(html).toContain("System → Remote Play");
    expect(html).toContain("Restart the PS5");
    expect(html).toContain("Sign out");
    expect(html).toContain("np-signin-open-profile");
    // Profile runs it; the card no longer repeats the manual recipe or links to
    // Payloads, which the web UI cannot open.
    expect(html).not.toContain("np-signin-download");
    expect(html).not.toContain("np-signin-open-payloads");
  });
});
