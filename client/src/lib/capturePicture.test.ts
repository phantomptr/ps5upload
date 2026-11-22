import { afterEach, describe, expect, it, vi } from "vitest";

import {
  baseName,
  captureAvailable,
  captureChoiceFor,
  captureConstraints,
  captureFailure,
  captureWanted,
  cleanChoice,
  findDevice,
  saveCaptureChoice,
  soundInputFor,
  type DeviceInfo,
} from "./capturePicture";
import { memoryStorage } from "../test-utils/storage";

const MAC = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)";
const ANDROID = "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Mobile";

function env(o: { tauri?: boolean; ua?: string; media?: boolean } = {}) {
  vi.stubGlobal("window", { localStorage: memoryStorage(), ...(o.tauri === false ? {} : { isTauri: true }) });
  vi.stubGlobal("navigator", {
    userAgent: o.ua ?? MAC,
    ...(o.media === false ? {} : { mediaDevices: { getUserMedia: () => {}, enumerateDevices: () => {} } }),
  });
}

afterEach(() => vi.unstubAllGlobals());

const dev = (kind: string, deviceId: string, label: string, groupId = ""): DeviceInfo => ({ kind, deviceId, label, groupId });

describe("where the option shows", () => {
  it("only in the desktop app with video inputs", () => {
    env();
    expect(captureAvailable()).toBe(true);
    env({ tauri: false });
    expect(captureAvailable()).toBe(false); // the browser build
    env({ ua: ANDROID });
    expect(captureAvailable()).toBe(false); // a phone
    env({ media: false });
    expect(captureAvailable()).toBe(false);
  });
});

describe("the choice, per console", () => {
  it("is kept per console and starts at Remote Play", () => {
    env();
    expect(captureChoiceFor("10.0.0.2")).toEqual({ picture: "rp", deviceId: "", label: "", sound: "rp" });
    saveCaptureChoice("10.0.0.2", { picture: "device", deviceId: "abc", label: "HD60", sound: "device" });
    expect(captureChoiceFor("10.0.0.2")).toEqual({ picture: "device", deviceId: "abc", label: "HD60", sound: "device" });
    expect(captureChoiceFor("10.0.0.3").picture).toBe("rp");
    expect(captureWanted("10.0.0.2")).toBe(true);
    expect(captureWanted("10.0.0.3")).toBe(false);
    expect(captureWanted(null)).toBe(false);
  });

  it("is not wanted without a device, or where it cannot show", () => {
    env();
    saveCaptureChoice("h", { picture: "device", deviceId: "", label: "", sound: "rp" });
    expect(captureWanted("h")).toBe(false);
    saveCaptureChoice("h", { picture: "device", deviceId: "x", label: "", sound: "rp" });
    env({ ua: ANDROID }); // a fresh storage too, but the phone alone says no
    expect(captureWanted("h")).toBe(false);
  });

  it("cleans a stored entry it does not understand", () => {
    expect(cleanChoice({ picture: "camera" as never, deviceId: 3 as never, sound: "both" as never })).toEqual({
      picture: "rp",
      deviceId: "",
      label: "",
      sound: "rp",
    });
  });
});

describe("finding the device again", () => {
  const list = [
    dev("audioinput", "default", "Default"),
    dev("videoinput", "v1", "FaceTime HD Camera", "g1"),
    dev("videoinput", "v2", "USB Video (1234:5678)", "g2"),
    dev("audioinput", "a1", "MacBook Microphone", "g0"),
    dev("audioinput", "a2", "Digital Audio Interface (USB Video)", "g2"),
  ];

  it("by id, else by name, else none", () => {
    expect(findDevice(list, { deviceId: "v2", label: "" })?.label).toBe("USB Video (1234:5678)");
    expect(findDevice(list, { deviceId: "gone", label: "FaceTime HD Camera" })?.deviceId).toBe("v1");
    expect(findDevice(list, { deviceId: "gone", label: "gone" })).toBeNull();
    // Never an audio input.
    expect(findDevice(list, { deviceId: "a2", label: "" })).toBeNull();
  });

  it("its sound: the same device's input, else the one named after it", () => {
    expect(soundInputFor(list, list[2])?.deviceId).toBe("a2");
    const noGroups = list.map((d) => ({ ...d, groupId: "" }));
    expect(soundInputFor(noGroups, noGroups[2])?.deviceId).toBe("a2");
    // A camera with no sound of its own gets none (never the computer's mic by accident).
    expect(soundInputFor(noGroups, noGroups[1])).toBeNull();
    expect(baseName("Digital Audio Interface (USB Video)")).toBe("usb");
    expect(baseName("USB Video (1234:5678)")).toBe("usb");
  });

  it("asks for that device's best picture, and its sound untouched", () => {
    const c = captureConstraints(list[2], list[4]);
    expect(c.video).toMatchObject({ deviceId: { exact: "v2" }, frameRate: { ideal: 60 } });
    expect(c.audio).toMatchObject({ deviceId: { exact: "a2" }, echoCancellation: false, noiseSuppression: false, autoGainControl: false });
    expect(captureConstraints(list[2], null).audio).toBe(false);
  });

  it("says why it did not open", () => {
    const err = (name: string) => Object.assign(new Error("x"), { name });
    expect(captureFailure(err("NotAllowedError"))).toBe("denied");
    expect(captureFailure(err("NotFoundError"))).toBe("missing");
    expect(captureFailure(err("NotReadableError"))).toBe("busy");
    expect(captureFailure(err("TypeError"))).toBe("other");
    expect(captureFailure("nope")).toBe("other");
  });
});
