import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import { Toggle } from "./Toggle";

// There is no DOM environment here (vitest runs in node, no jsdom and no
// testing-library), so these render to static markup and assert on the
// accessibility contract rather than driving clicks.
const html = (props: Parameters<typeof Toggle>[0]) =>
  renderToStaticMarkup(<Toggle {...props} />);

describe("Toggle — switch semantics", () => {
  it("is a switch, not a checkbox", () => {
    const out = html({ checked: false, onChange: () => {}, label: "Beta" });
    expect(out).toContain('role="switch"');
    expect(out).toContain('type="button"');
    expect(out).not.toContain('type="checkbox"');
  });

  it("reports its state through aria-checked", () => {
    expect(html({ checked: true, onChange: () => {}, label: "Beta" })).toContain(
      'aria-checked="true"',
    );
    expect(
      html({ checked: false, onChange: () => {}, label: "Beta" }),
    ).toContain('aria-checked="false"');
  });

  // The label is associated by id, not by wrapping — the button is a sibling of
  // the text. If aria-labelledby ever stopped pointing at the rendered label,
  // the switch would announce as an unlabelled button.
  it("points aria-labelledby at the label it renders", () => {
    const out = html({ checked: false, onChange: () => {}, label: "Beta" });
    const id = /aria-labelledby="([^"]+)"/.exec(out)?.[1];
    expect(id).toBeTruthy();
    expect(out).toContain(`id="${id}"`);
    expect(out).toContain("Beta");
  });
});

describe("Toggle — hint", () => {
  it("describes the switch only when a hint is given", () => {
    const withHint = html({
      checked: false,
      onChange: () => {},
      label: "Beta",
      hint: "Turns on unfinished screens.",
    });
    expect(withHint).toContain("aria-describedby=");
    expect(withHint).toContain("Turns on unfinished screens.");

    const without = html({ checked: false, onChange: () => {}, label: "Beta" });
    expect(without).not.toContain("aria-describedby");
  });

  // The hint was widened from string to ReactNode so call sites can keep their
  // inline emphasis instead of flattening it into plain text.
  it("accepts a ReactNode hint and keeps its markup", () => {
    const hint = "Helps some titles.";
    const out = html({
      checked: false,
      onChange: () => {},
      label: "Also patch libc.prx",
      hint: <strong>{hint}</strong>,
    });
    expect(out).toMatch(/<p[^>]*>\s*<strong>Helps some titles\.<\/strong>/);
  });
});

describe("Toggle — disabled", () => {
  // Disabled must not mean "off": the control still shows the underlying state
  // it would return to, which is why aria-checked stays live.
  it("marks the button disabled while still reflecting checked", () => {
    const out = html({
      checked: true,
      onChange: () => {},
      label: "Beta",
      disabled: true,
    });
    expect(out).toContain("disabled");
    expect(out).toContain('aria-checked="true"');
  });
});
