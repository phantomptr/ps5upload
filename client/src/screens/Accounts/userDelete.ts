/** What the helper refuses a user delete with (payload/include/user_guard.h). */
export type UserDeleteRefusal = "signed_in" | "state_unknown";

/** The refusal an error from `userDelete` carries, if any: the engine passes the helper's word on
 *  inside its own message. */
export function userDeleteRefusal(error: unknown): UserDeleteRefusal | null {
  const text = String(error);
  if (text.includes("user_signed_in")) return "signed_in";
  if (text.includes("user_state_unknown")) return "state_unknown";
  return null;
}
