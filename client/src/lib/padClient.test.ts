import { describe, expect, it } from "vitest";

import { readStatus } from "./padClient";

describe("pad link status", () => {
  it("tells a phone the PS5 waits for the passcode on the computer, nothing more", () => {
    const s = readStatus(
      JSON.stringify({ stage: "joined", session: { host: "192.168.1.100", playing: false, passcode: true } }),
    );
    expect(s).toEqual({ kind: "joined", host: "192.168.1.100", playing: false, passcode: true });
  });

  it("an older engine without the field reads as not waiting", () => {
    const s = readStatus(JSON.stringify({ stage: "joined", session: { host: "h", playing: true } }));
    expect(s).toEqual({ kind: "joined", host: "h", playing: true, passcode: false });
  });
});
