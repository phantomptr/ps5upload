import { describe, expect, it } from "vitest";

import { looksLikeMacLocalNetworkBlock } from "./localNetworkHint";

const MAC =
  "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15";
const WIN = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36";

describe("a probe that macOS itself refused", () => {
  it("is recognised on a Mac from 'No route to host (os error 65)'", () => {
    expect(
      looksLikeMacLocalNetworkBlock("No route to host (os error 65)", MAC),
    ).toBe(true);
    expect(
      looksLikeMacLocalNetworkBlock(
        "connect 192.168.1.5:9021: No route to host (os error 65)",
        MAC,
      ),
    ).toBe(true);
  });

  it("is not claimed for other failures on a Mac", () => {
    expect(
      looksLikeMacLocalNetworkBlock("Connection refused (os error 61)", MAC),
    ).toBe(false);
    expect(looksLikeMacLocalNetworkBlock("timeout", MAC)).toBe(false);
    expect(looksLikeMacLocalNetworkBlock(undefined, MAC)).toBe(false);
  });

  it("is not claimed on other systems, where 65 or 'no route' means something else", () => {
    expect(
      looksLikeMacLocalNetworkBlock("No route to host (os error 65)", WIN),
    ).toBe(false);
    expect(
      looksLikeMacLocalNetworkBlock(
        "No route to host (os error 113)",
        "X11; Linux x86_64",
      ),
    ).toBe(false);
  });
});
