/** A wall time as `HH:MM`, for "As of 14:38" and "last reached 14:38". */
export function clock(ms: number): string {
  return new Date(ms).toLocaleTimeString("en-GB", { hour: "2-digit", minute: "2-digit" });
}
