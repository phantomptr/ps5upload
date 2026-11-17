import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { KrakenStatus } from "../../api/ps5";
import { KrakenUpdateNote } from "./KrakenUpdateNote";

const k = (version: string | null) => ({ version }) as unknown as KrakenStatus;

describe("the Kraken Mount update note", () => {
  it("asks for 0.2.1 when an older one runs", () => {
    const html = renderToStaticMarkup(<KrakenUpdateNote kraken={k("0.2.0")} />);
    expect(html).toContain("kraken-update-note");
    expect(html).toContain("0.2.0");
    expect(html).toContain("0.2.1");
  });
  it("says nothing for a current one", () => {
    expect(renderToStaticMarkup(<KrakenUpdateNote kraken={k("0.2.1")} />)).toBe("");
    expect(renderToStaticMarkup(<KrakenUpdateNote kraken={null} />)).toBe("");
  });
});
