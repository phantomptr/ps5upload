/** Which greeting fits the hour (0-23, local time): morning until noon,
 *  afternoon until six, evening otherwise (late night included — "good
 *  morning" at 2 a.m. reads wrong). */
export function greetingPart(hour: number): "morning" | "afternoon" | "evening" {
  if (hour >= 5 && hour < 12) return "morning";
  if (hour >= 12 && hour < 18) return "afternoon";
  return "evening";
}
