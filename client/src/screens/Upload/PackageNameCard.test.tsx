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

import type { PkgNames } from "../../api/pkgName";
import { PackageNameView } from "./PackageNameCard";
import type { NameFlowState } from "./pkgNameFlow";

const noop = () => {};
const names = (over: Partial<PkgNames> = {}): PkgNames => ({
  platform: "ps5",
  content_id: "UP0000-PPSA01234_00-TESTGAME00000000",
  name: "A Game-example.invalid",
  entries: [{ key: "en-US", name: "A Game-example.invalid" }],
  other_names: 0,
  refusal: null,
  max_bytes: 127,
  ...over,
});
const state = (over: Partial<NameFlowState> = {}): NameFlowState => ({
  path: "/games/a.pkg",
  loading: false,
  names: names(),
  value: "A Game-example.invalid",
  allLanguages: false,
  saving: false,
  saved: null,
  error: null,
  ...over,
});
const render = (s: NameFlowState, disabled = false) =>
  renderToStaticMarkup(
    <PackageNameView state={s} disabled={disabled} onValue={noop} onAllLanguages={noop} onSave={noop} />,
  );
const saveButton = (html: string) => /<button[^>]*data-testid="pkg-name-save"[^>]*>/.exec(html)?.[0] ?? "";

describe("the Name on the PS5 card", () => {
  it("shows the package's name in a field, with Change name off until it differs", () => {
    const html = render(state());
    expect(html).toContain("Name on the PS5");
    expect(html).toContain('value="A Game-example.invalid"');
    expect(html).toContain('maxLength="127"');
    expect(saveButton(html)).toContain('disabled=""');
    expect(html).not.toContain("every language");
    expect(saveButton(render(state({ value: "A Game" })))).not.toContain('disabled=""');
  });

  it("offers every language only when some have another name", () => {
    const html = render(state({ names: names({ other_names: 2 }) }));
    expect(html).toContain("Use this name in every language (other names in the package: 2)");
  });

  it("the name field takes the name's own direction (a Latin name in an Arabic app)", () => {
    const html = render(state());
    expect(/<input[^>]*data-testid="pkg-name-input"[^>]*>/.exec(html)?.[0]).toContain('dir="auto"');
  });

  it("is locked while the package is being sent", () => {
    const html = render(state({ value: "A Game" }), true);
    expect(saveButton(html)).toContain('disabled=""');
    expect(/<input[^>]*data-testid="pkg-name-input"[^>]*>/.exec(html)?.[0]).toContain('disabled=""');
  });

  it("says the name changed, and why when it did not", () => {
    const saved = { platform: "ps5" as const, previous: "x", name: "A Game", changed: 1, written_bytes: 9 };
    expect(render(state({ saved }))).toContain("Name changed.");
    expect(render(state({ saved: { ...saved, changed: 0 } }))).not.toContain("Name changed.");
    const failed = render(state({ error: { during: "save", reason: "a PS5 is installing from this package" } }));
    expect(failed).toContain("The name was not changed: a PS5 is installing from this package");
  });

  it("shows the bytes left while the name is changed, and says so itself when it is too long", () => {
    const roomy = names({ room: { default_name: 30, all_languages: 30 } });
    // Untouched: no counter.
    const idle = render(state({ names: roomy }));
    expect(idle).not.toContain("pkg-name-room");
    expect(idle).not.toContain("pkg-name-too-long");
    // Changed and fitting: how many bytes are left, and Change name can be pressed.
    const fits = render(state({ names: roomy, value: "A Game" }));
    expect(fits).toContain("Bytes left for the name: 24");
    expect(saveButton(fits)).not.toContain('disabled=""');
    // Too long (33 bytes in 11 letters): said here, and Change name is off.
    const over = render(state({ names: roomy, value: "テ".repeat(11) }));
    expect(over).toContain('data-testid="pkg-name-too-long"');
    expect(over).toContain('role="alert"');
    expect(over).toContain("This name is too long for this package. Bytes that fit: 30. Bytes over: 3.");
    expect(over).toContain('aria-invalid="true"');
    expect(over).not.toContain("Bytes left for the name");
    expect(saveButton(over)).toContain('disabled=""');
  });

  it("an official package shows its name and why it stays", () => {
    const html = render(
      state({ names: names({ refusal: { code: "retail", reason: "this is an official package" } }) }),
    );
    expect(html).toContain("A Game-example.invalid");
    expect(html).toContain("This is an official package, so its name can&#x27;t be changed");
    expect(html).not.toContain("pkg-name-input");
    expect(html).not.toContain("pkg-name-save");
    const other = render(
      state({ names: names({ refusal: { code: "unsupported", reason: "the body digest does not hold" } }) }),
    );
    expect(other).toContain("This package&#x27;s name can&#x27;t be changed: the body digest does not hold");
  });

  it("shows nothing before the name is read, for a package with no name and for a delta update", () => {
    expect(render(state({ names: null, loading: true }))).toBe("");
    expect(render(state({ names: null, error: { during: "read", reason: "not a package" } }))).toBe("");
    for (const code of ["patch", "no_name"]) {
      expect(render(state({ names: names({ name: "", refusal: { code, reason: "" } }) }))).toBe("");
    }
  });
});
