// Electron adapter for event API

export type UnlistenFn = () => void;

export interface Event<T> {
  payload: T;
}

export async function listen<T>(
  event: string,
  callback: (event: Event<T>) => void
): Promise<UnlistenFn> {
  const webApi = (window as unknown as {
    ps5uploadWebAPI?: {
      on?: (event: string, cb: (payload: T) => void) => UnlistenFn;
    };
  }).ps5uploadWebAPI;

  if (window.electronAPI?.on) {
    const unlisten = window.electronAPI.on(event, (payload: T) => {
      callback({ payload });
    });
    return Promise.resolve(unlisten);
  }

  if (webApi?.on) {
    const unlisten = webApi.on(event, (payload: T) => {
      callback({ payload });
    });
    return Promise.resolve(unlisten);
  }

  console.warn(`Event listener not available for: ${event}`);
  return Promise.resolve(() => {});
}

export async function once<T>(
  event: string,
  callback: (event: Event<T>) => void
): Promise<void> {
  const webApi = (window as unknown as {
    ps5uploadWebAPI?: {
      once?: (event: string, cb: (payload: T) => void) => void;
    };
  }).ps5uploadWebAPI;

  if (window.electronAPI?.once) {
    window.electronAPI.once(event, (payload: T) => {
      callback({ payload });
    });
    return Promise.resolve();
  }

  if (webApi?.once) {
    webApi.once(event, (payload: T) => {
      callback({ payload });
    });
    return Promise.resolve();
  }

  console.warn(`Event once not available for: ${event}`);
  return Promise.resolve();
}
