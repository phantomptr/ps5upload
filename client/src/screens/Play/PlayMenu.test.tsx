import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/remotePlay", async () => {
  const { create } = await import("zustand");
  return {
    useRemotePlayStore: create(() => ({
      stream: null,
      target: null,
      prefs: { volume: 1, muted: false },
      pip: null,
      pictureHere: false,
      setPictureHere: () => {},
      setPrefs: () => {},
    })),
  };
});
vi.mock("./MicControl", () => ({ MicTile: () => null }));

import { PlayMenu } from "./PlayMenu";

const noop = () => {};
const menu = (extra: Partial<Parameters<typeof PlayMenu>[0]>) =>
  renderToStaticMarkup(
    <PlayMenu
      inFull={false}
      pipKind={null}
      onClose={noop}
      onExitFull={noop}
      onPip={noop}
      onSettings={noop}
      onStop={noop}
      onGame={noop}
      {...extra}
    />,
  );

describe("PlayMenu: Stream on a phone (#731)", () => {
  it("has a Stream tile where the engine can go live (the web UI in a phone's Safari)", () => {
    const out = menu({ onGoLive: noop });
    expect(out).toContain('data-testid="rp-menu-golive"');
    expect(out).not.toContain('data-testid="rp-menu-golive-hint"');
  });

  it("says where Go live runs in the phone app, instead of a tile", () => {
    const out = menu({ goLiveHint: true });
    expect(out).not.toContain('data-testid="rp-menu-golive"');
    expect(out).toContain("To go live from here, run the engine on a computer and open its web UI in Safari");
  });

  it("shows neither in full screen, where the menu has no room", () => {
    const out = menu({ inFull: true, onGoLive: noop, goLiveHint: true });
    expect(out).not.toContain("rp-menu-golive");
  });
});
