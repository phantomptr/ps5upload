import { describe, expect, it } from "vitest";
import { onConsoleBrowser, phoneUrl } from "./PhoneHandoff";

describe("continue on your phone", () => {
  it("is this page on the console's LAN address while LAN access is on", () => {
    expect(phoneUrl({ lan: true, addr: "192.0.2.99" }, "9180")).toBe("http://192.0.2.99:9180/");
    expect(phoneUrl({ lan: true, addr: "10.0.0.7" }, "")).toBe("http://10.0.0.7:9180/");
  });

  it("shows nothing when LAN access is off, the helper is older, or the address is not usable", () => {
    expect(phoneUrl({ lan: false, addr: "192.0.2.99" }, "9180")).toBeNull();
    expect(phoneUrl({ lan: true }, "9180")).toBeNull(); // an older helper: no addr
    expect(phoneUrl({ lan: true, addr: "" }, "9180")).toBeNull();
    expect(phoneUrl({ lan: true, addr: "127.0.0.1" }, "9180")).toBeNull();
    expect(phoneUrl({ lan: true, addr: "evil.example/x?" }, "9180")).toBeNull();
    expect(phoneUrl(null, "9180")).toBeNull();
  });

  it("is offered only in the console's own browser, not on a phone or PC already", () => {
    expect(onConsoleBrowser("127.0.0.1")).toBe(true);
    expect(onConsoleBrowser("localhost")).toBe(true);
    expect(onConsoleBrowser("192.0.2.99")).toBe(false);
    expect(onConsoleBrowser("")).toBe(false);
  });
});
