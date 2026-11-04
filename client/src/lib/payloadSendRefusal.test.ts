import { describe, expect, it } from "vitest";

import { PayloadSendRefused } from "../api/ps5";
import { payloadSendRefusalText } from "./payloadSendRefusal";

describe("a refused payload send", () => {
  it("says why, by the engine's reason", () => {
    expect(payloadSendRefusalText(new PayloadSendRefused("pkg_install_busy"))).toContain("install");
    expect(payloadSendRefusalText(new PayloadSendRefused("helper_busy"))).toContain("transfer");
    expect(payloadSendRefusalText(new PayloadSendRefused("loader_busy"))).toContain("loader");
  });

  it("leaves any other error as it is", () => {
    expect(payloadSendRefusalText(new Error("connect failed"))).toBe("connect failed");
  });
});
