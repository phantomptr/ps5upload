import { describe, expect, it } from "vitest";
import { KEY_ASK_GAP_MS, KeyAsker } from "./watchPlayer";

describe("KeyAsker (#648)", () => {
  it("asks at once, then at most once per gap", () => {
    let t = 0;
    let asks = 0;
    const k = new KeyAsker(() => asks++, () => t);
    k.waiting();
    expect(asks).toBe(1);
    t = 400;
    k.waiting();
    expect(asks).toBe(1);
    t = KEY_ASK_GAP_MS;
    k.waiting();
    expect(asks).toBe(2);
  });

  it("does nothing without an ask (a watcher)", () => {
    expect(() => new KeyAsker(undefined).waiting()).not.toThrow();
  });
});
