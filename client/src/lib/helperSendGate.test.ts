import { beforeEach, describe, expect, it } from "vitest";

import { resetHelperSendGate, sendHelperOnce, sentAgoMs } from "./helperSendGate";

describe("one helper send per console at a time", () => {
  beforeEach(() => resetHelperSendGate());

  it("a second send while one is running joins it instead of starting another helper", async () => {
    let sends = 0;
    let release!: () => void;
    const slow = () =>
      new Promise<void>((r) => {
        sends++;
        release = r;
      });
    const a = sendHelperOnce("192.168.1.10", slow);
    const b = sendHelperOnce("192.168.1.10:9021", slow);
    await new Promise((r) => setTimeout(r, 0));
    release();
    expect(await a).toBe("sent");
    expect(await b).toBe("joined");
    expect(sends).toBe(1);
  });

  it("other consoles send on their own", async () => {
    let sends = 0;
    const quick = async () => {
      sends++;
    };
    await Promise.all([sendHelperOnce("192.168.1.10", quick), sendHelperOnce("192.168.1.11", quick)]);
    expect(sends).toBe(2);
  });

  it("remembers when a console was last sent a helper, and a failed send is not a send", async () => {
    expect(sentAgoMs("192.168.1.10")).toBeNull();
    await sendHelperOnce("192.168.1.10", async () => {});
    expect(sentAgoMs("192.168.1.10")).toBeLessThan(1000);
    await expect(
      sendHelperOnce("192.168.1.11", async () => {
        throw new Error("connect 9021 refused");
      }),
    ).rejects.toThrow("refused");
    expect(sentAgoMs("192.168.1.11")).toBeNull();
  });
});
