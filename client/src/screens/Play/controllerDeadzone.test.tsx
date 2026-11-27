import { afterEach, describe, expect, it, vi } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { memoryStorage } from "../../test-utils/storage";

/** The window the screen reads: storage, where the page is, and events it may listen to. */
function installWindowStub(): void {
  vi.stubGlobal("window", {
    localStorage: memoryStorage(),
    location: { origin: "http://127.0.0.1:19113", pathname: "/", search: "", hash: "" },
    addEventListener: () => {},
    removeEventListener: () => {},
  });
}

const ID = "Xbox 360 Controller (XInput STANDARD GAMEPAD)";

function stubPad(): void {
  const g = {
    id: ID,
    index: 0,
    connected: true,
    mapping: "standard",
    axes: [0, 0, 0.05, -0.83],
    buttons: new Array(17).fill(0).map(() => ({ pressed: false, touched: false, value: 0 })),
    timestamp: 0,
  };
  vi.stubGlobal("navigator", { getGamepads: () => [g] });
}

describe("Controllers: stick dead zones", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("offers dead zones for each controller and shows the saved ones", async () => {
    installWindowStub();
    stubPad();
    const { saveDeadzones } = await import("../../lib/padProfiles");
    saveDeadzones(ID, { left: 0, right: 0.87 });
    const { ControllerSetup } = await import("./ControllerSetup");
    const out = renderToStaticMarkup(<ControllerSetup />);
    expect(out).toContain('data-testid="rp-pad-deadzone"');
    expect(out).toContain('data-testid="rp-pad-deadzone-status"');
    expect(out).toContain("left 0%, right 87%");
  });

  it("says nothing about dead zones for a controller without them", async () => {
    installWindowStub();
    stubPad();
    const { ControllerSetup } = await import("./ControllerSetup");
    const out = renderToStaticMarkup(<ControllerSetup />);
    expect(out).toContain('data-testid="rp-pad-deadzone"');
    expect(out).not.toContain("rp-pad-deadzone-status");
  });
});
