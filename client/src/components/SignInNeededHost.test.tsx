import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { SignInNeededActions, SignInNeededBody, SignInNeededHost } from "./SignInNeededHost";
import { dismissSignInNeeded } from "../lib/signInNeeded";

describe("SignInNeededHost", () => {
  beforeEach(() => dismissSignInNeeded());

  it("renders nothing while nothing is waiting (a signed-in user changes nothing)", () => {
    expect(renderToStaticMarkup(<SignInNeededHost />)).toBe("");
  });

  it("offers Sign in when Wake & sign in is set up", () => {
    const noop = () => {};
    expect(renderToStaticMarkup(<SignInNeededBody canSignIn />)).toContain("Sign in from here");
    expect(renderToStaticMarkup(<SignInNeededActions canSignIn onSignIn={noop} onClose={noop} />)).toContain(
      "signin-needed-go",
    );
  });

  it("says to sign in on the PS5 without the Remote Play keys", () => {
    const noop = () => {};
    expect(renderToStaticMarkup(<SignInNeededBody canSignIn={false} />)).toContain("Sign in on the PS5, then try again.");
    expect(
      renderToStaticMarkup(<SignInNeededActions canSignIn={false} onSignIn={noop} onClose={noop} />),
    ).not.toContain("signin-needed-go");
  });
});
