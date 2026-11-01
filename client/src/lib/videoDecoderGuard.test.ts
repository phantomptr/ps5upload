import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// Issue #610: the Linux app primes WebKitGTK's GL display and leaves a crash marker around
// every decoder it asks for; elsewhere it does nothing.
const calls: Array<[string, unknown]> = [];
let possible = true;
vi.mock("./invokeLogged", () => ({
  invoke: vi.fn(async (cmd: string, args: unknown) => {
    calls.push([cmd, args]);
  }),
}));
vi.mock("./linuxGraphics", () => ({ linuxGraphicsPossible: () => possible }));
const pushNotification = vi.fn();
vi.mock("../state/notifications", () => ({
  pushNotification: (...a: unknown[]) => pushNotification(...a),
}));
const toastPush = vi.fn();
vi.mock("../state/toasts", () => ({
  useToastStore: { getState: () => ({ push: toastPush }) },
}));
vi.mock("./trStatic", () => ({
  trStatic: (_k: string, fallback: string) => fallback,
}));

const guard = await import("./videoDecoderGuard");

/** A document whose canvases count WebGL contexts made and lost. */
function fakeDocument() {
  const made = { webgl: 0, lost: 0 };
  vi.stubGlobal("document", {
    createElement: () => ({
      getContext: (kind: string) => {
        if (kind !== "webgl") return null;
        made.webgl++;
        return { getExtension: () => ({ loseContext: () => made.lost++ }) };
      },
    }),
  });
  return made;
}

const flush = () => new Promise((r) => setTimeout(r, 0));

describe("videoDecoderGuard", () => {
  beforeEach(() => {
    guard.resetVideoDecoderGuardForTests();
    calls.length = 0;
    possible = true;
    pushNotification.mockReset();
    toastPush.mockReset();
  });
  afterEach(() => vi.unstubAllGlobals());

  it("makes one WebGL context before the first decoder, then never again", async () => {
    const made = fakeDocument();
    await guard.withDecoderStart(async () => {
      expect(made.webgl).toBe(1);
    });
    await guard.withDecoderStart(async () => undefined);
    expect(made).toEqual({ webgl: 1, lost: 1 });
  });

  it("writes the marker before the decoder is asked for and removes it after", async () => {
    fakeDocument();
    const order: string[] = [];
    await guard.withDecoderStart(async () => {
      order.push(`asked with ${JSON.stringify(calls)}`);
    });
    await flush();
    expect(order).toEqual([
      `asked with ${JSON.stringify([["linux_video_starting", { on: true }]])}`,
    ]);
    expect(calls).toEqual([
      ["linux_video_starting", { on: true }],
      ["linux_video_starting", { on: false }],
    ]);
  });

  it("keeps the marker while any attempt is open, and ends each only once", async () => {
    fakeDocument();
    const a = guard.decoderStarting();
    const b = guard.decoderStarting();
    await Promise.all([a.ready, b.ready]);
    a.end();
    a.end();
    await flush();
    expect(calls).toEqual([["linux_video_starting", { on: true }]]);
    b.end();
    await flush();
    expect(calls[calls.length - 1]).toEqual([
      "linux_video_starting",
      { on: false },
    ]);
    expect(calls).toHaveLength(2);
  });

  it("removes the marker when the check throws", async () => {
    fakeDocument();
    await expect(
      guard.withDecoderStart(async () => {
        throw new Error("no");
      }),
    ).rejects.toThrow("no");
    await flush();
    expect(calls[calls.length - 1]).toEqual([
      "linux_video_starting",
      { on: false },
    ]);
  });

  it("does nothing outside the Linux app", async () => {
    possible = false;
    const made = fakeDocument();
    expect(await guard.withDecoderStart(async () => 7)).toBe(7);
    await flush();
    expect(calls).toEqual([]);
    expect(made.webgl).toBe(0);
  });

  it("reads software decoding from the launch flag", () => {
    vi.stubGlobal("window", {});
    expect(guard.linuxSoftwareVideo()).toBe(false);
    vi.stubGlobal("window", {
      __PS5UPLOAD_LINUX_GRAPHICS__: { softwareVideo: true },
    });
    expect(guard.linuxSoftwareVideo()).toBe(true);
    vi.stubGlobal("window", {
      __PS5UPLOAD_LINUX_GRAPHICS__: { softwareVideo: "yes" },
    });
    expect(guard.linuxSoftwareVideo()).toBe(false);
  });

  it("says once that the picture is now decoded in software after a crash", () => {
    vi.stubGlobal("window", {
      __PS5UPLOAD_LINUX_GRAPHICS__: { videoRecovered: true },
    });
    expect(guard.announceVideoRecovery()).toBe(true);
    expect(guard.announceVideoRecovery()).toBe(false);
    expect(pushNotification).toHaveBeenCalledTimes(1);
    const [level, title, extras] = pushNotification.mock.calls[0];
    expect(level).toBe("warning");
    expect(title).toBe("Picture now decoded in software");
    expect(extras.body).toMatch(
      /^Play crashed while starting the picture last time/,
    );
    expect(extras.body).toContain("Hardware video decoding");
    expect(toastPush).toHaveBeenCalledTimes(1);
  });

  it("says nothing on a normal launch", () => {
    vi.stubGlobal("window", {
      __PS5UPLOAD_LINUX_GRAPHICS__: { videoRecovered: false },
    });
    expect(guard.announceVideoRecovery()).toBe(false);
    vi.stubGlobal("window", {});
    expect(guard.announceVideoRecovery()).toBe(false);
    expect(pushNotification).not.toHaveBeenCalled();
  });
});
