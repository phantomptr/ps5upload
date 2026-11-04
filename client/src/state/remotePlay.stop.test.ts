import { describe, expect, it, vi } from "vitest";

const close = vi.fn(async () => ({}));
vi.mock("../screens/Play/playTogetherState", () => ({ joinApi: { close } }));

describe("stopping Play", () => {
  it("ends Play Together on that console at once, so friends aren't left waiting", async () => {
    const { useRemotePlayStore } = await import("./remotePlay");
    const stop = vi.fn();
    useRemotePlayStore.setState({
      stream: { stop } as never,
      target: { host: "192.168.1.5", name: "PS5", registKey: "", rpKey: "" },
    });
    useRemotePlayStore.getState().stop();
    expect(stop).toHaveBeenCalled();
    await vi.waitFor(() => expect(close).toHaveBeenCalledWith("192.168.1.5"));
  });

  it("does nothing to Play Together when nothing streams", async () => {
    close.mockClear();
    const { useRemotePlayStore } = await import("./remotePlay");
    useRemotePlayStore.setState({ stream: null, target: { host: "192.168.1.5", name: "PS5", registKey: "", rpKey: "" } });
    useRemotePlayStore.getState().stop();
    await new Promise((r) => setTimeout(r, 20));
    expect(close).not.toHaveBeenCalled();
  });
});
