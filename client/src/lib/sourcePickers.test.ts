import { describe, expect, it } from "vitest";

// Which Browse buttons reach saved servers. A picker that reads a source gets the ▾ server menu
// (<BrowseButton … remote …>); a picker that chooses where to SAVE stays local until saving to a
// server exists, because every consumer of its path writes to local disk.
// Read the screens as raw text at transform time, like browserInvokeCoverage.test.ts does.
const SOURCES = import.meta.glob("../screens/**/*.tsx", {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;
const src = (p: string) => {
  const text = SOURCES[`../${p}`];
  if (text === undefined) throw new Error(`no source ${p}`);
  return text;
};
const remoteButtons = (text: string) => (text.match(/<BrowseButton[^>]*?\bremote\b/gs) ?? []).length;

describe("source pickers reach saved servers", () => {
  it.each([
    ["screens/Upload/index.tsx", 2],
    ["screens/InstallPackage/index.tsx", 1],
    ["screens/FileSystem/index.tsx", 1],
    ["screens/Payloads/SendPanel.tsx", 1],
    ["screens/Payloads/PlaylistsPanel.tsx", 1],
    ["screens/FpkgConvert/GameCard.tsx", 2],
  ])("%s offers servers on its source picker", (file, n) => {
    expect(remoteButtons(src(file))).toBe(n);
  });

  it.each([
    "screens/Screenshots/index.tsx",
    "screens/Videos/index.tsx",
    "screens/BugReport/index.tsx",
    "screens/Settings/index.tsx",
    "screens/Search/index.tsx",
    "screens/Logs/AppLogsPanel.tsx",
    "screens/Stats/index.tsx",
    "screens/Saves/index.tsx",
    "screens/LocalImage/index.tsx",
    "screens/Upload/FfpkgInspectorPanel.tsx",
  ])("%s keeps local pickers", (file) => {
    expect(remoteButtons(src(file))).toBe(0);
  });
});
