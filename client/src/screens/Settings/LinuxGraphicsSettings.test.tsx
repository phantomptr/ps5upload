import { isValidElement, type ReactElement, type ReactNode } from "react";
import { describe, expect, it, vi } from "vitest";

vi.mock("../../state/lang", async (orig) => ({
  ...(await orig<typeof import("../../state/lang")>()),
  useTr: () => (key: string, _vars?: unknown, fallback?: string) =>
    fallback ?? key,
}));

const { LinuxGraphicsView } = await import("./LinuxGraphicsSettings");
const { Toggle } = await import("../../components/Toggle");
import type { GraphicsStatus } from "../../lib/linuxGraphics";

const tr = ((key: string, _vars?: unknown, fallback?: string) =>
  fallback ?? key) as Parameters<typeof LinuxGraphicsView>[0]["tr"];

const status = (
  over: Partial<GraphicsStatus> = {},
  launch: Partial<GraphicsStatus["launch"]> = {},
): GraphicsStatus => ({
  linux: true,
  saved: false,
  softwareVideoSaved: false,
  ...over,
  launch: {
    compositingOff: false,
    dmabufOff: true,
    safeMode: false,
    currentStack: false,
    setByApp: true,
    softwareVideo: false,
    videoRecovered: false,
    ...launch,
  },
});

function elements(node: ReactNode): ReactElement<Record<string, unknown>>[] {
  const out: ReactElement<Record<string, unknown>>[] = [];
  const walk = (n: ReactNode) => {
    if (Array.isArray(n)) return n.forEach(walk);
    if (!isValidElement(n)) return;
    const el = n as ReactElement<Record<string, unknown>>;
    out.push(el);
    walk(el.props.children as ReactNode);
  };
  walk(node);
  return out;
}

function view(s: GraphicsStatus, onHardwareVideo = vi.fn()) {
  const tree = LinuxGraphicsView({
    status: s,
    busy: false,
    error: null,
    tr,
    onSafeMode: vi.fn(),
    onHardwareVideo,
  });
  const els = elements(tree);
  const hw = els.find(
    (e) => e.type === Toggle && e.props.label === "Hardware video decoding",
  )!;
  const text = JSON.stringify(
    els.map((e) => e.props.children).filter((c) => typeof c === "string"),
  );
  return { hw, text };
}

describe("Settings › Window › Graphics › Hardware video decoding (#610)", () => {
  it("is on by default, and turning it off saves software decoding", () => {
    const onHardwareVideo = vi.fn();
    const { hw } = view(status(), onHardwareVideo);
    expect(hw.props.checked).toBe(true);
    expect(hw.props.disabled).toBe(false);
    expect(String(hw.props.hint)).toMatch(/Applies after restart/);
    (hw.props.onChange as (on: boolean) => void)(false);
    expect(onHardwareVideo).toHaveBeenCalledWith(false);
  });

  it("shows off when software decoding is saved (by the user or after a crash)", () => {
    expect(
      view(status({ softwareVideoSaved: true }, { softwareVideo: true })).hw
        .props.checked,
    ).toBe(false);
  });

  it("is off and locked while Safe graphics mode is on", () => {
    const { hw } = view(
      status({ saved: true }, { safeMode: true, softwareVideo: true }),
    );
    expect(hw.props.checked).toBe(false);
    expect(hw.props.disabled).toBe(true);
  });

  it("asks for a restart until the launch matches the saved choice", () => {
    expect(view(status({ softwareVideoSaved: true })).text).toContain(
      "Restart PS5Upload to apply.",
    );
    expect(
      view(status({ softwareVideoSaved: true }, { softwareVideo: true })).text,
    ).not.toContain("Restart PS5Upload to apply.");
    expect(view(status()).text).not.toContain("Restart PS5Upload to apply.");
  });
});
