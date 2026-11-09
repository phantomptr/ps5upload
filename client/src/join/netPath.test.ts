import { describe, expect, it } from "vitest";

import { hostPath, pathOf, reachRank, selectedPath, urlPath } from "./netPath";

describe("which way the connection goes (the engine's rule, join_net/rtc.rs)", () => {
  it("ranks the home network, then Tailscale, then public addresses", () => {
    for (const home of ["192.168.0.2", "10.1.2.3", "172.20.0.1", "169.254.3.4", "127.0.0.1", "fd00::1", "fe80::1", "[fe80::1]"]) {
      expect(reachRank(home), home).toBe(0);
    }
    for (const tail of ["100.64.0.1", "100.127.255.254", "fd7a:115c:a1e0::1"]) expect(reachRank(tail), tail).toBe(1);
    for (const pub of ["100.63.0.1", "100.128.0.1", "8.8.8.8", "172.32.0.1", "2001:db8::1"]) expect(reachRank(pub), pub).toBe(2);
    for (const none of [null, "", "f1e2d3c4.local", "999.1.1.1"]) expect(reachRank(none), String(none)).toBeNull();
  });

  it("names the path of a pair as the host's panel does", () => {
    expect(pathOf("abc.local", "192.168.1.10")).toBe("lan");
    expect(pathOf("100.100.0.2", "100.100.0.1")).toBe("tailscale");
    expect(pathOf("10.9.0.5", "203.0.113.1")).toBe("internet");
    expect(pathOf("192.168.1.20", null)).toBeNull();
  });

  it("reads ICE's selected pair from the stats (Chrome's transport, Firefox's selected flag)", () => {
    const pair = (extra: Record<string, unknown>) => ({ type: "candidate-pair", id: "P", localCandidateId: "L", remoteCandidateId: "R", ...extra });
    const cands = (remote: string) => [
      { type: "local-candidate", id: "L", address: "abc.local" },
      { type: "remote-candidate", id: "R", ip: remote },
    ];
    expect(selectedPath([{ type: "transport", id: "T", selectedCandidatePairId: "P" }, pair({}), ...cands("192.168.1.10")])).toBe("lan");
    expect(selectedPath([pair({ selected: true }), ...cands("203.0.113.1")])).toBe("internet");
    expect(selectedPath([pair({ nominated: true, state: "succeeded" }), ...cands("100.101.2.3")])).toBe("tailscale");
    expect(selectedPath([pair({ state: "in-progress" }), ...cands("192.168.1.10")])).toBeNull();
    // A Map-like report (what getStats gives).
    const m = new Map<string, Record<string, unknown>>([
      ["P", pair({ selected: true })],
      ["L", cands("x")[0]],
      ["R", cands("10.0.0.2")[1]],
    ]);
    expect(selectedPath(m as unknown as { forEach(cb: (s: Record<string, unknown>) => void): void })).toBe("lan");
  });

  it("names a plain socket's path by its host", () => {
    expect(hostPath("192.168.1.10")).toBe("lan");
    expect(hostPath("my-pc.local")).toBe("lan");
    expect(hostPath("100.101.2.3")).toBe("tailscale");
    expect(hostPath("my-pc.tail1234.ts.net")).toBe("tailscale");
    expect(hostPath("ps5.example.com")).toBeNull();
    expect(urlPath("http://192.168.1.10:19113")).toBe("lan");
    expect(urlPath("not a url")).toBeNull();
  });
});
