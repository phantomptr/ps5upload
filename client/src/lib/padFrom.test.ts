import { describe, expect, it } from "vitest";

import { describePads, padActive, padFromMessage } from "./padFrom";
import { IDLE_PAD } from "./remotePlayStream";

const pad = (id: string, mapping = "standard") => ({ id, mapping, connected: true }) as unknown as Gamepad;

describe("what feeds the pad, for the engine's controller check", () => {
  it("names each controller's reader and layout", () => {
    const pads = describePads([null, pad("DualSense (app)"), pad("Xbox", "")], () => "standard");
    expect(pads).toEqual([
      { id: "DualSense (app)", mapping: "standard", profile: "standard", reader: "native" },
      { id: "Xbox", mapping: "none", profile: "standard", reader: "browser" },
    ]);
  });

  it("changes its key only when the description changes", () => {
    const a = padFromMessage(new Set(["keys", "controller"]), []);
    const b = padFromMessage(new Set(["controller", "keys"]), []);
    expect(a.key).toBe(b.key);
    expect(a.value.inputs).toEqual(["controller", "keys"]);
    expect(padFromMessage(new Set(["controller"]), []).key).not.toBe(a.key);
  });

  it("counts a half-pulled trigger as input", () => {
    expect(padActive(IDLE_PAD)).toBe(false);
    expect(padActive({ ...IDLE_PAD, l2: 128 })).toBe(true);
  });
});
