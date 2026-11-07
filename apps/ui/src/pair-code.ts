// The pairing code (docs/contracts/api.md, "Pairing codes"): `pf1-` and lower-case base32 of
// version (1 byte), secret (10), SPKI hash (32) and the listen address (UTF-8). The UI reads the
// address only, to name it in a message. It never keeps the secret, and it never logs the code.
const ALPHABET = "abcdefghijklmnopqrstuvwxyz234567";
export const CODE_PREFIX = "pf1-";
const HEAD_BYTES = 1 + 10 + 32;

/** A client reads a code with surrounding white space and either letter case. */
export function normalizeCode(raw: string): string {
  return raw.replace(/\s+/g, "").toLowerCase();
}

export function hasCodePrefix(raw: string): boolean {
  const code = normalizeCode(raw);
  return code.startsWith(CODE_PREFIX) && code.length > CODE_PREFIX.length;
}

/** The listen address the code names, or `null` when the code does not decode. */
export function addressFromCode(raw: string): string | null {
  const code = normalizeCode(raw);
  if (!code.startsWith(CODE_PREFIX)) return null;
  const bytes: number[] = [];
  let acc = 0;
  let bits = 0;
  for (const ch of code.slice(CODE_PREFIX.length)) {
    const v = ALPHABET.indexOf(ch);
    if (v < 0) return null;
    acc = (acc << 5) | v;
    bits += 5;
    if (bits >= 8) {
      bytes.push((acc >> (bits - 8)) & 0xff);
      bits -= 8;
      acc &= (1 << bits) - 1;
    }
  }
  if (bytes[0] !== 1 || bytes.length <= HEAD_BYTES) return null;
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(new Uint8Array(bytes.slice(HEAD_BYTES)));
  } catch {
    return null;
  }
}
