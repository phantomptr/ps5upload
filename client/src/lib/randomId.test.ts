import { describe, expect, it, vi } from "vitest";

import { randomHexId } from "./randomId";

describe("randomHexId", () => {
  it("uses randomUUID when the secure-context API is available", () => {
    const source = {
      randomUUID: () => "12345678-1234-4234-9234-123456789abc",
    } as unknown as Crypto;
    expect(randomHexId(source)).toBe("12345678123442349234123456789abc");
  });

  it("works on plain-HTTP LAN pages where randomUUID is unavailable", () => {
    const getRandomValues = vi.fn((bytes: Uint8Array) => {
      bytes.forEach((_, i) => {
        bytes[i] = i;
      });
      return bytes;
    });
    const source = { getRandomValues } as unknown as Crypto;

    expect(randomHexId(source)).toBe("000102030405060708090a0b0c0d0e0f");
    expect(getRandomValues).toHaveBeenCalledOnce();
  });

  it("always returns the 32 hex characters required by the FTX2 tx id", () => {
    const id = randomHexId(undefined);
    expect(id).toMatch(/^[0-9a-f]{32}$/);
  });
});
