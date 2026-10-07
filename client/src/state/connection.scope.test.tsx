import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, beforeEach } from "vitest";

import {
  ConnectionScope,
  connectionSnapshotFor,
  useConnectionStore,
} from "./connection";

const A = "192.168.0.5";
const B = "192.168.0.6";

function Shows() {
  const host = useConnectionStore((s) => s.host);
  const status = useConnectionStore((s) => s.payloadStatus);
  return <div>{`${host} ${status}`}</div>;
}

const shown = (host: string, frozen: boolean) =>
  renderToStaticMarkup(
    <ConnectionScope host={host} frozen={frozen}>
      <Shows />
    </ConnectionScope>,
  );

describe("a console's screens kept behind another console", () => {
  beforeEach(() => {
    useConnectionStore.getState().setHost(A);
    useConnectionStore.getState().setHostStatus(A, { payloadStatus: "up" });
  });

  it("remembers each console's state as it last was while selected", () => {
    useConnectionStore.getState().setHost(B);
    expect(connectionSnapshotFor(A)).toMatchObject({ host: A, payloadStatus: "up" });
    expect(connectionSnapshotFor(B)).toMatchObject({ host: B });
    expect(connectionSnapshotFor("10.0.0.9")).toBeNull();
  });

  it("keeps showing its own console while another one is selected", () => {
    // The user selects console B: A's screens go behind it and must not turn into B's.
    useConnectionStore.getState().setHost(B);
    useConnectionStore.getState().setHostStatus(B, { payloadStatus: "down" });
    expect(shown(A, true)).toBe(`<div>${A} up</div>`);
    expect(shown(B, true)).toBe(`<div>${B} down</div>`);
  });

  it("is transparent for the console on show, and with no scope at all", () => {
    // Server rendering reads a zustand store's initial state, so only "same as unscoped"
    // can be asserted here; the browser test covers the live console.
    expect(shown(A, false)).toBe(renderToStaticMarkup(<Shows />));
  });

  it("leaves the store's own getState live", () => {
    useConnectionStore.getState().setHost(B);
    expect(useConnectionStore.getState().host).toBe(B);
  });
});
