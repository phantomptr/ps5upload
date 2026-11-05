import { create } from "zustand";

/**
 * A question an install has for the person, from code that has no screen of its own (installs run
 * from the console queue). One at a time, in order; `InstallAskHost` shows them.
 *
 * With no screen mounted, or no answer in time, the answer is no: the install ends with its
 * reason and the rest of the queue goes on, so nothing is replaced with nobody there.
 */
export interface InstallAsk {
  title: string;
  message: string;
  confirmLabel: string;
  destructive?: boolean;
}

interface Pending extends InstallAsk {
  resolve: (yes: boolean) => void;
}

interface InstallAskState {
  hosts: number;
  pending: Pending[];
}

export const INSTALL_ASK_TIMEOUT_MS = 3 * 60 * 1000;

export const useInstallAskStore = create<InstallAskState>(() => ({ hosts: 0, pending: [] }));

export function askInstall(ask: InstallAsk): Promise<boolean> {
  if (useInstallAskStore.getState().hosts === 0) return Promise.resolve(false);
  return new Promise((resolve) => {
    useInstallAskStore.setState((s) => ({ pending: [...s.pending, { ...ask, resolve }] }));
  });
}

/** Answers the oldest question. */
export function answerInstallAsk(yes: boolean): void {
  const [first, ...rest] = useInstallAskStore.getState().pending;
  if (!first) return;
  useInstallAskStore.setState({ pending: rest });
  first.resolve(yes);
}

/** A screen can show the questions from now on; the returned function ends that. */
export function registerInstallAskHost(): () => void {
  useInstallAskStore.setState((s) => ({ hosts: s.hosts + 1 }));
  return () => {
    useInstallAskStore.setState((s) => ({ hosts: Math.max(0, s.hosts - 1) }));
    if (useInstallAskStore.getState().hosts === 0) {
      for (const p of useInstallAskStore.getState().pending) p.resolve(false);
      useInstallAskStore.setState({ pending: [] });
    }
  };
}
