import { beforeEach, describe, expect, it } from "vitest";

import { answerInstallAsk, askInstall, registerInstallAskHost, useInstallAskStore } from "./installAsk";

const ask = { title: "t", message: "m", confirmLabel: "c" };

describe("install questions", () => {
  beforeEach(() => useInstallAskStore.setState({ hosts: 0, pending: [] }));

  it("answers no when no screen can show the question", async () => {
    await expect(askInstall(ask)).resolves.toBe(false);
  });

  it("asks in order and takes each answer", async () => {
    const end = registerInstallAskHost();
    const a = askInstall({ ...ask, title: "a" });
    const b = askInstall({ ...ask, title: "b" });
    expect(useInstallAskStore.getState().pending[0].title).toBe("a");
    answerInstallAsk(true);
    await expect(a).resolves.toBe(true);
    expect(useInstallAskStore.getState().pending[0].title).toBe("b");
    answerInstallAsk(false);
    await expect(b).resolves.toBe(false);
    end();
  });

  it("answers no for what is left when the screen goes", async () => {
    const end = registerInstallAskHost();
    const a = askInstall(ask);
    end();
    await expect(a).resolves.toBe(false);
    expect(useInstallAskStore.getState().pending).toEqual([]);
  });
});
