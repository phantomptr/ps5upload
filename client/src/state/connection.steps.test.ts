import { beforeEach, describe, expect, it } from "vitest";
import { useConnectionStore } from "./connection";

// The Connection screen's two check steps are one pair of fields shown for the selected console.
// A check started on console A that answers after the user switched to B must not write A's
// result ("Port 9021 is open on <A>") into B's screen.
describe("connection check steps", () => {
  beforeEach(() => {
    useConnectionStore.getState().setHost("192.168.0.5");
  });

  it("a step result for the selected console is shown", () => {
    useConnectionStore.getState().setStep1For("192.168.0.5", "ok", "Port 9021 is open on 192.168.0.5");
    expect(useConnectionStore.getState().step1).toBe("ok");
  });

  it("a step result for a console that is no longer selected is dropped", () => {
    useConnectionStore.getState().setHost("192.168.0.6");
    useConnectionStore.getState().setStep1For("192.168.0.5", "ok", "Port 9021 is open on 192.168.0.5");
    useConnectionStore.getState().setStep2For("192.168.0.5", "ok", "Helper is running on 192.168.0.5");
    const s = useConnectionStore.getState();
    expect(s.step1).toBe("idle");
    expect(s.step2).toBe("idle");
    expect(s.step1Msg).not.toContain("192.168.0.5");
  });

  it("matches the console by host, whatever port was typed", () => {
    useConnectionStore.getState().setStep1For("192.168.0.5:9021", "ok", "ok");
    expect(useConnectionStore.getState().step1).toBe("ok");
  });
});
