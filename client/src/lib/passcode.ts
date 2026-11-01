/** A PS5 user passcode: four digits. */
export function isPasscode(s: string): boolean {
  return /^[0-9]{4}$/.test(s);
}
