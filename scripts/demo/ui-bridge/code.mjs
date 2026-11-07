// The pairing code (docs/contracts/api.md, "Pairing codes"): `pf1-` and lower-case base32 of
// version (1 byte), secret (10 bytes), SPKI hash (32 bytes) and the listen address (UTF-8).
// The bridge needs the hash and the address. It never keeps the secret.
const ALPHABET = "abcdefghijklmnopqrstuvwxyz234567";
export const CODE_PREFIX = "pf1-";
const HEAD_BYTES = 1 + 10 + 32;

/** A client reads a code with surrounding white space and either letter case. */
export function normalizeCode(raw) {
  return String(raw).replace(/\s+/g, "").toLowerCase();
}

/** `{ spkiHex, address }`, or `null` when the code does not decode. */
export function decodeCode(raw) {
  const code = normalizeCode(raw);
  if (!code.startsWith(CODE_PREFIX)) return null;
  const bytes = [];
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
  const spkiHex = Buffer.from(bytes.slice(11, HEAD_BYTES)).toString("hex");
  let address;
  try {
    address = new TextDecoder("utf-8", { fatal: true }).decode(Buffer.from(bytes.slice(HEAD_BYTES)));
  } catch {
    return null;
  }
  return parseAddress(address) ? { spkiHex, address } : null;
}

/** `host:port` or `[v6]:port` as `{ host, port }`, or `null`. */
export function parseAddress(address) {
  const m = /^\[([0-9a-fA-F:.]+)\]:(\d{1,5})$/.exec(address) ?? /^([A-Za-z0-9._-]+):(\d{1,5})$/.exec(address);
  if (!m) return null;
  const port = Number(m[2]);
  if (port < 1 || port > 65535) return null;
  return { host: m[1], port };
}
