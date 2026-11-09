// The cost of a chat in weighted units (wu).
//
// "wu" is defined by MASTER-SPEC §2 step 7a: "weighted units (input 1, output 5, cache read 0.1,
// 5-minute write 1.25, 1-hour write 2)". The app brief says: "dollars for API-priced sessions, wu for
// subscription sessions; the two never add up into one number". The chat runs the operator's
// subscription login (Q1), so the UI shows wu and keeps the CLI's dollar figure for a tooltip.
//
// Known limit: `ChatUsage` sums the cache writes of all turns without the 5-minute and 1-hour split,
// so a session whose turns mixed the two is weighted at its last turn's TTL.
import type { ChatUsage } from "@pulse-factory/api-client";

/** `input + 5·output + 0.1·cache_read + w·cache_write`, with `w` 2 for a 1-hour TTL and 1.25 otherwise. */
export function weightedUnits(usage: ChatUsage, cacheTtlSeconds: number | null): number {
  const write = cacheTtlSeconds === 3600 ? 2 : 1.25;
  return (
    usage.input_tokens +
    5 * usage.output_tokens +
    0.1 * usage.cache_read_tokens +
    write * usage.cache_write_tokens
  );
}

/** `<n> wu` under 1000, `<n.n>k wu` under a million, `<n.n>M wu` above. */
export function formatWu(units: number): string {
  const whole = Math.round(units);
  if (whole < 1000) return `${whole} wu`;
  const thousands = (whole / 1000).toFixed(1);
  if (Number(thousands) < 1000) return `${thousands}k wu`;
  return `${(whole / 1_000_000).toFixed(1)}M wu`;
}

/** The header's Cost text for one session record. */
export function costLabel(usage: ChatUsage, cacheTtlSeconds: number | null): string {
  return formatWu(weightedUnits(usage, cacheTtlSeconds));
}

/** The CLI's dollar figure, for the Cost element's tooltip. */
export function dollarsTitle(usage: ChatUsage): string {
  return `$${usage.cost_micros / 1e6} at API prices`;
}
