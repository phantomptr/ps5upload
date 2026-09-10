import { describe, expect, it } from "vitest";

/**
 * Screen sources, read through Vite rather than node:fs — the client
 * tsconfig ships no node types, so a fs import typechecks in vitest but
 * fails `tsc --noEmit`, which is the gate that matters.
 */
const sources = import.meta.glob("../screens/**/*.tsx", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

function read(rel: string): string {
  const key = Object.keys(sources).find((k) => k.endsWith(`/${rel}`));
  if (!key) throw new Error(`no source for ${rel}`);
  return sources[key];
}

/**
 * Values a screen ships pre-filled must be valid before anyone touches
 * them.
 *
 * Two shipped defaults were broken at once and neither was caught by a
 * test, because tests exercise the values *they* choose while a user
 * gets the one in the box:
 *
 *   - SmbBrowser defaulted to "smb://192.168.1.100:445". The backend
 *     takes a socket address, not a URL, so it tried to DNS-resolve that
 *     literal string and failed in 50 ms — before touching the network,
 *     on a subnet almost nobody is on.
 *   - SdkChanger defaulted to 0x09060000, which is not a firmware.
 *     Versions are BCD, so 9.60 is 0x09600000.
 *
 * These assert against the source text rather than rendering, because
 * the property under test is "what ships", not "what renders".
 */
describe("shipped defaults are usable as-is", () => {
  it("Backport ships the approved SDK target", () => {
    const src = read("InstalledApps/BackportPanel.tsx");
    expect(src).toContain('sdkPatch(titleId, "0x04000031"');
  });

  it("Backport keeps the risky libc patch opt-in", () => {
    const src = read("InstalledApps/BackportPanel.tsx");
    expect(src).toContain("useState(false)");
  });

  it("SmbBrowser ships an empty server rather than an unusable example", () => {
    const src = read("SmbBrowser/index.tsx");
    const m = /const \[server, setServer\] = useState\((.*?)\);/.exec(src);
    expect(m, "SmbBrowser should declare a server default").not.toBeNull();
    // Empty is correct: the placeholder shows the shape instead. A
    // pre-filled value has to be one the backend can parse.
    expect(m![1].trim()).toBe('""');
  });

  it("FtpServer's default port avoids the port ftpsrv already uses", () => {
    const src = read("FtpServer/index.tsx");
    const m = /const \[port, setPort\] = useState\((\d+)\)/.exec(src);
    expect(m, "FtpServer should declare a numeric port default").not.toBeNull();
    const port = Number(m![1]);
    // 2121 is ftpsrv.elf's default, and ftpsrv ships in our own payload
    // catalogue — sharing it produced bind_failed out of the box.
    expect(port).not.toBe(2121);
    expect(port).toBeGreaterThan(1024);
    expect(port).toBeLessThan(65536);
  });

  it("every PS5 path default is absolute", () => {
    // A relative default would resolve against whatever the payload's
    // cwd happens to be, which is not a thing a user can reason about.
    for (const [file, re] of [
      ["DiskUsage/index.tsx", /const \[path, setPath\] = useState\("(.*?)"\)/],
      ["Shell/index.tsx", /const \[cwd, setCwd\] = useState\("(.*?)"\)/],
      ["SmbBrowser/index.tsx", /const \[destRoot, setDestRoot\] = useState\("(.*?)"\)/],
      ["FtpServer/index.tsx", /const \[root, setRoot\] = useState\("(.*?)"\)/],
    ] as const) {
      const m = re.exec(read(file));
      expect(m, `${file} should declare its path default`).not.toBeNull();
      expect(m![1].startsWith("/"), `${file} default "${m![1]}" must be absolute`).toBe(
        true,
      );
    }
  });
});
