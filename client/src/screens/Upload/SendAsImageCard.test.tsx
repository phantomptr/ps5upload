import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { MemoryRouter } from "react-router";

// "Send it as one game image" builds the image on this computer first: in Workshop's Output
// folder (the same setting), not always in ~/Downloads/fpkgs. A server render reads a store's
// first state, so the saved folder is put where the store reads it before the store loads.
const KEY_DIR = "ps5upload.convert.output_dir";

function stubStorage(dir: string | null) {
  const items = new Map<string, string>(dir === null ? [] : [[KEY_DIR, dir]]);
  const storage = {
    getItem: (k: string) => items.get(k) ?? null,
    setItem: (k: string, v: string) => void items.set(k, v),
    removeItem: (k: string) => void items.delete(k),
    clear: () => items.clear(),
    key: () => null,
    length: items.size,
  };
  vi.stubGlobal("localStorage", storage);
  const url = new URL("http://localhost/upload");
  vi.stubGlobal("window", {
    localStorage: storage,
    location: { origin: url.origin, href: url.href, protocol: url.protocol, host: url.host, hostname: url.hostname, port: url.port, pathname: url.pathname, search: "", hash: "" },
    addEventListener: () => {},
    removeEventListener: () => {},
  });
}

async function render(dir: string | null) {
  vi.resetModules();
  stubStorage(dir);
  const { SendAsImageCard } = await import("./SendAsImageCard");
  return renderToStaticMarkup(
    <MemoryRouter>
      <SendAsImageCard sourcePath="/games/PPSA01234" destinationVolume={null} destinationSubpath="" />
    </MemoryRouter>,
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("send as one image: where it is built", () => {
  it("shows Workshop's Output folder, and a way to change it", async () => {
    const out = await render("D:\\Downloads");
    expect(out).toContain('data-testid="upload-image-output"');
    expect(out).toContain("D:\\Downloads");
    expect(out).toContain("Change");
  });

  it("says the default when no Output folder is set", async () => {
    expect(await render(null)).toContain("Default: ~/Downloads/fpkgs");
  });
});
