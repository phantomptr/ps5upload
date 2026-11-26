/**
 * How long a PS5 user's name can be (#940). The console keeps the name in a 17-byte field, the last
 * byte its end mark, so the limit is 16 BYTES of UTF-8, not 16 letters: "é" takes 2, "日" takes 3,
 * most emoji 4. The helper and the engine refuse a longer name (they never cut it, which could split
 * a letter); checking here first lets the screen say why in the user's language.
 * Keep in sync with PROFILE_NAME_BYTES_MAX (payload/include/profile.h) and USER_NAME_MAX_BYTES
 * (engine/crates/ps5upload-core/src/profile.rs).
 */
export const USER_NAME_MAX_BYTES = 16;

/** The UTF-8 size of `name`. */
export function userNameBytes(name: string): number {
  return new TextEncoder().encode(name).length;
}

/** Whether `name` is too long for the console to keep whole. */
export function userNameTooLong(name: string): boolean {
  return userNameBytes(name) > USER_NAME_MAX_BYTES;
}
