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
const direct = (text: string) => (text.match(/<BrowseButton[^>]*?\bremote\b/gs) ?? []).length;
// The shared picker (components/AddSources.tsx) is a "Choose file" and a "Choose folder", both
// reaching servers: a screen that renders it gets those two.
const ADD_SOURCES = (
  import.meta.glob("../components/AddSources.tsx", { query: "?raw", import: "default", eager: true }) as Record<
    string,
    string
  >
)["../components/AddSources.tsx"];
const remoteButtons = (text: string) =>
  direct(text) + (text.match(/<AddSources\b/g) ?? []).length * direct(ADD_SOURCES ?? "");

describe("source pickers reach saved servers", () => {
  it("the shared picker offers servers on both of its buttons", () => {
    expect(direct(ADD_SOURCES ?? "")).toBe(2);
  });

  it.each([
    // The drop zone draws its sources once full size and once as the slim row; only one shows.
    ["screens/Upload/index.tsx", 4],
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
    "screens/Search/SearchPanel.tsx",
    "screens/Logs/AppLogsPanel.tsx",
    "screens/Activity/StatsPanel.tsx",
    "screens/Saves/index.tsx",
    "screens/LocalImage/LocalDrives.tsx",
    "screens/Upload/FfpkgInspectorPanel.tsx",
  ])("%s keeps local pickers", (file) => {
    expect(remoteButtons(src(file))).toBe(0);
  });
});
