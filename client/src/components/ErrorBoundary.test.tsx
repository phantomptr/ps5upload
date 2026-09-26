import { describe, expect, it } from "vitest";

import { RootErrorBoundary } from "./ErrorBoundary";

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
});
