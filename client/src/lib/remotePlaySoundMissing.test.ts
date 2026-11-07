import { describe, expect, it } from "vitest";
import { SOUND_MISSING_AFTER_S, soundMissingFrom } from "./remotePlayStream";

describe("soundMissingFrom", () => {
  it("says nothing for an engine that doesn't report sound", () => {
    expect(soundMissingFrom({ frames: 60 }, 99)).toBeUndefined();
  });
  it("says nothing while sound arrives and plays", () => {
    expect(soundMissingFrom({ frames: 60, audio_frames_total: 12 }, 99, true)).toBeUndefined();
  });
  it("says this device plays none when sound arrives but never played", () => {
    expect(soundMissingFrom({ frames: 60, audio_frames_total: 600 }, SOUND_MISSING_AFTER_S, false)).toBe("device");
  });
  it("waits a few seconds of picture before saying the PS5 sends none", () => {
    const st = { frames: 60, audio_frames_total: 0, audio_other_codec: 0 };
    expect(soundMissingFrom(st, SOUND_MISSING_AFTER_S - 1)).toBeUndefined();
    expect(soundMissingFrom(st, SOUND_MISSING_AFTER_S)).toBe("none");
  });
  it("tells sound in another format apart from none", () => {
    expect(soundMissingFrom({ frames: 60, audio_frames_total: 0, audio_other_codec: 40 }, SOUND_MISSING_AFTER_S)).toBe("format");
  });
});
