import { describe, expect, it } from "vitest";

import { LINK_MODES, linkModeFacts } from "./linkModes";

describe("linkModeFacts", () => {
  it("only asks to keep the computer awake when the computer does the downloading", () => {
    expect(linkModeFacts("direct")).toMatchObject({
      downloader: "ps5",
      computerMustStayAwake: false,
      needsDiskHere: false,
      progressHere: false,
    });
    expect(linkModeFacts("stream")).toMatchObject({
      downloader: "computer",
      computerMustStayAwake: true,
      needsDiskHere: false,
      progressHere: true,
    });
    expect(linkModeFacts("download")).toMatchObject({
      downloader: "computer",
      computerMustStayAwake: true,
      needsDiskHere: true,
      progressHere: true,
    });
  });

  it("the certificate check is ours to skip only when this computer downloads", () => {
    // In direct mode the PS5 does its own TLS handshake: the option cannot apply.
    expect(linkModeFacts("direct").certificateCheckApplies).toBe(false);
    expect(linkModeFacts("stream").certificateCheckApplies).toBe(true);
    expect(linkModeFacts("download").certificateCheckApplies).toBe(true);
  });

  it("offers the default first", () => {
    expect(LINK_MODES[0]).toBe("stream");
    expect([...LINK_MODES].sort()).toEqual(["direct", "download", "stream"]);
  });
});
