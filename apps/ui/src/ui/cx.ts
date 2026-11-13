/** Joins class names, leaving out the parts that are false or empty. */
export function cx(...parts: Array<string | false | null | undefined>): string {
  return parts.filter((part): part is string => typeof part === "string" && part !== "").join(" ");
}
