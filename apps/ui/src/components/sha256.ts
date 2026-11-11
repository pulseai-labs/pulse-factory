// The SHA-256 of a text's UTF-8 bytes as 64 lower-case hex digits, by Web Crypto (r1.s4.w3, W8). A
// confirm code for an answer binds this hash, so the controller never sees the text until the spend.
export async function sha256Hex(text: string): Promise<string> {
  const bytes = new TextEncoder().encode(text);
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
}
