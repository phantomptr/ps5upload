import { describe, expect, it } from "vitest";
import { allowLine } from "./Tailscale";

describe("the console page shows the PS5's Tailscale allow-list, read-only", () => {
  it("names the listed devices, or says every device, or nobody yet", () => {
    const base = { enabled: true, state: "running", peers: [{ name: "phone-ps5u.tail1.ts.net.", ip: "100.64.0.5" }] };
    expect(allowLine({ ...base, allow_mode: "listed", allow: "100.64.0.5,100.64.0.6" })).toContain("phone-ps5u, 100.64.0.6");
    expect(allowLine({ ...base, allow_mode: "all" })).toContain("Every device");
    expect(allowLine({ ...base, allow_mode: "listed", allow: "" })).toContain("Only devices you add");
    // An older helper says nothing of a list.
    expect(allowLine(base)).toBeNull();
  });
});
