import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { RootErrorBoundary, SectionErrorBoundary, toError } from "./ErrorBoundary";

/**
 * Unit tests for the static lifecycle method. Verifying the `render`
 * branch needs a React renderer (we'd add @testing-library/react for
 * that) — the static method is a pure transform we can test directly.
 *
 * The contract that matters: `getDerivedStateFromError` must capture
 * the error so React shows the fallback UI on the *next* render. If
 * it ever returns a non-Error or accidentally nullifies the `err`
 * field, the boundary silently swallows crashes — exactly the
 * regression this guard exists to prevent.
 */
describe("RootErrorBoundary", () => {
  it("getDerivedStateFromError captures the thrown Error", () => {
    const err = new Error("boom");
    const next = RootErrorBoundary.getDerivedStateFromError(err);
    expect(next.err).toBe(err);
    expect(next.componentStack).toBe("");
  });

  it("getDerivedStateFromError handles a TypeError subclass", () => {
    // React surfaces any thrown value, including subclasses. Common
    // case: a downstream `Cannot read property 'x' of undefined` is a
    // TypeError. The boundary must still capture it; if a future
    // refactor narrowed the parameter to `Error` only, TypeErrors
    // would slip through (they ARE Error instances, but a refactor
    // could break this).
    const err = new TypeError("nope");
    const next = RootErrorBoundary.getDerivedStateFromError(err);
    expect(next.err).toBe(err);
    expect(next.err).toBeInstanceOf(Error);
  });

  it("the fallback's three buttons wrap on a narrow phone instead of squeezing", () => {
    // At 360 px they shared one row: "Reload window" broke after every few letters and
    // "Report this crash" ran off the card.
    const boundary = new RootErrorBoundary({ children: null });
    boundary.state = { err: new Error("boom"), componentStack: "" };
    const html = renderToStaticMarkup(<>{boundary.render()}</>);
    const actions = html.match(/<div class="([^"]*)" data-testid="errorboundary-actions">/);
    expect(actions?.[1].split(" ")).toEqual(expect.arrayContaining(["flex", "flex-wrap"]));
    expect(html).toContain("boom");
  });
});

describe("a thrown value that is not an Error", () => {
  it("still counts as an error, so the boundary shows its message instead of a white window", () => {
    // `throw null` kept as `err: null` reads as "no error": the boundary renders the children
    // again, they throw again, and React unmounts the whole tree.
    for (const thrown of [null, undefined, "text", 42, { code: 7 }]) {
      const root = RootErrorBoundary.getDerivedStateFromError(thrown);
      expect(root.err).toBeInstanceOf(Error);
      const section = SectionErrorBoundary.getDerivedStateFromError(thrown);
      expect(section.err).toBeInstanceOf(Error);
    }
    expect(toError("boom").message).toBe("boom");
    expect(toError(null).message).toContain("null");
    expect(toError({ code: 7 }).message).toContain('{"code":7}');
    const circular: Record<string, unknown> = {};
    circular.self = circular;
    expect(toError(circular)).toBeInstanceOf(Error);
    const err = new TypeError("x");
    expect(toError(err)).toBe(err);
  });
});

describe("SectionErrorBoundary", () => {
  it("renders its children while nothing failed", () => {
    const html = renderToStaticMarkup(
      <SectionErrorBoundary logName="restore card">
        <i data-ok="1" />
      </SectionErrorBoundary>,
    );
    expect(html).toBe("<i data-ok=\"1\"></i>");
  });

  it("shows the failure in its own place with Try again, not the whole-window crash screen", () => {
    const b = new SectionErrorBoundary({ logName: "restore card", children: <i data-ok="1" /> });
    b.state = SectionErrorBoundary.getDerivedStateFromError(new Error("Cannot read properties of undefined"));
    const html = renderToStaticMarkup(<>{b.render()}</>);
    expect(html).toContain('data-testid="section-error"');
    expect(html).toContain("The rest of the app still works");
    expect(html).toContain("Cannot read properties of undefined");
    expect(html).toContain("Try again");
    expect(html).not.toContain("data-ok");
    expect(html).not.toContain("errorboundary-actions");
  });

  it("clears the error when what it shows changes (another game, another screen)", () => {
    const b = new SectionErrorBoundary({ logName: "x", resetKey: "PPSA00001", children: null });
    b.state = { err: new Error("boom") };
    let next: unknown = "unset";
    b.setState = ((s: unknown) => {
      next = s;
    }) as typeof b.setState;
    // Same key: kept.
    b.componentDidUpdate({ logName: "x", resetKey: "PPSA00001", children: null });
    expect(next).toBe("unset");
    // Another key: cleared, so the new content renders.
    (b as unknown as { props: unknown }).props = { logName: "x", resetKey: "PPSA00002", children: null };
    b.componentDidUpdate({ logName: "x", resetKey: "PPSA00001", children: null });
    expect(next).toEqual({ err: null });
  });
});
