import { describe, expect, it } from "vitest";

import { loaderHint } from "./helperSendRuntime";

describe("loaderHint", () => {
  it("turns a refused loader port into what to do, keeping the original error", () => {
    const raw = "connect 192.168.1.128:9021: Connection refused (os error 111)";
    const msg = loaderHint(raw);
    expect(msg).toContain("loader (port 9021) isn't running");
    expect(msg).toContain(raw);
  });

  it("leaves any other error as it is", () => {
    expect(loaderHint("the engine has no helper to send")).toBe(
      "the engine has no helper to send",
    );
  });
});
