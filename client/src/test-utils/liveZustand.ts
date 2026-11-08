// zustand under `renderToStaticMarkup` renders each store's INITIAL state (React's server
// snapshot), so a test's `setState` before rendering is invisible to the screen. Mocking zustand
// with this makes every `create()` hook read the store's live state instead (no subscription:
// a static render reads once). Use in a test file:
//
//   vi.mock("zustand", async (orig) => liveZustand(await orig<typeof import("zustand")>()));
import type * as Zustand from "zustand";

type Init = Zustand.StateCreator<unknown>;

export function liveZustand(real: typeof Zustand): typeof Zustand {
  const make = (init: Init) => {
    const api = real.createStore(init);
    const hook = <T>(sel: (s: unknown) => T = (s) => s as T) => sel(api.getState());
    return Object.assign(hook, api);
  };
  const create = ((init?: Init) => (init ? make(init) : make)) as unknown as typeof real.create;
  return { ...real, create };
}
