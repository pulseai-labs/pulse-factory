// One HTTPS/1.1 request to the controller's API with the server key pinned.
//
// The API's certificate is self-signed and has no chain to check, so the pin is the only check:
//   1. connect with TLS 1.3 and no chain check, and wait for the handshake;
//   2. take the peer's SubjectPublicKeyInfo and compare its SHA-256 with the pinned hash. The SPKI
//      comes from the raw certificate (`getPeerCertificate().raw`, read by `X509Certificate`), not
//      from `getPeerCertificate().pubkey`: for an EC key `pubkey` is the bare 65-byte point, not the
//      DER SubjectPublicKeyInfo that the server hashes (ADR-0012 uses ECDSA P-256);
//   3. on a mismatch, destroy the socket. **Not one byte has been written to it by then.**
//   4. only then write the request.
// Every call connects and checks again, so a key that changes later fails closed too.
import crypto from "node:crypto";
import net from "node:net";
import tls from "node:tls";

export class PinError extends Error {}
export class NetworkError extends Error {}

const MAX_RESPONSE = 2 * 1024 * 1024;

function dechunk(buf) {
  const parts = [];
  let pos = 0;
  for (;;) {
    const eol = buf.indexOf("\r\n", pos);
    if (eol < 0) throw new NetworkError("truncated response");
    const size = Number.parseInt(buf.subarray(pos, eol).toString("latin1").split(";")[0].trim(), 16);
    if (!Number.isFinite(size) || size < 0) throw new NetworkError("bad chunk");
    pos = eol + 2;
    if (size === 0) return Buffer.concat(parts);
    if (pos + size > buf.length) throw new NetworkError("truncated response");
    parts.push(buf.subarray(pos, pos + size));
    pos += size + 2;
  }
}

function parseResponse(buf) {
  const sep = buf.indexOf("\r\n\r\n");
  if (sep < 0) throw new NetworkError("no response");
  const lines = buf.subarray(0, sep).toString("latin1").split("\r\n");
  const m = /^HTTP\/1\.[01] (\d{3})/.exec(lines[0] ?? "");
  if (!m) throw new NetworkError("not an HTTP response");
  const headers = {};
  for (const line of lines.slice(1)) {
    const i = line.indexOf(":");
    if (i > 0) headers[line.slice(0, i).trim().toLowerCase()] = line.slice(i + 1).trim();
  }
  let body = buf.subarray(sep + 4);
  if ((headers["transfer-encoding"] ?? "").toLowerCase().includes("chunked")) body = dechunk(body);
  else if (headers["content-length"] !== undefined) body = body.subarray(0, Number(headers["content-length"]));
  return { status: Number(m[1]), headers, body };
}

/**
 * @param {{host: string, port: number, pinHex: string, method: "GET"|"POST", path: string,
 *   headers?: Record<string, string>, body?: string, timeoutMs?: number}} o
 * @returns {Promise<{status: number, headers: Record<string, string>, body: Buffer}>}
 */
export function pinnedRequest(o) {
  const { host, port, pinHex, method, path, headers = {}, body, timeoutMs = 10_000 } = o;
  const want = Buffer.from(pinHex, "hex");
  return new Promise((resolve, reject) => {
    let settled = false;
    let verified = false;
    const chunks = [];
    let received = 0;
    const socket = tls.connect({
      host,
      port,
      minVersion: "TLSv1.3",
      rejectUnauthorized: false, // the pin decides, not a chain
      ...(net.isIP(host) ? {} : { servername: host }),
    });
    const finish = (fn, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket.destroy();
      fn(value);
    };
    const timer = setTimeout(() => finish(reject, new NetworkError("timed out")), timeoutMs);

    socket.once("secureConnect", () => {
      let spki;
      try {
        const raw = socket.getPeerCertificate().raw;
        spki = new crypto.X509Certificate(raw).publicKey.export({ type: "spki", format: "der" });
      } catch {
        spki = undefined;
      }
      if (!spki) return finish(reject, new NetworkError("no server key"));
      const got = crypto.createHash("sha256").update(spki).digest();
      if (got.length !== want.length || !crypto.timingSafeEqual(got, want)) {
        return finish(reject, new PinError("server key changed"));
      }
      verified = true;
      const payload = body === undefined ? null : Buffer.from(body, "utf8");
      const head = [
        `${method} ${path} HTTP/1.1`,
        `Host: ${host}:${port}`,
        "Accept: application/json",
        "Connection: close",
        ...Object.entries(headers).map(([k, v]) => `${k}: ${v}`),
      ];
      if (payload) head.push("Content-Type: application/json", `Content-Length: ${payload.length}`);
      socket.write(`${head.join("\r\n")}\r\n\r\n`);
      if (payload) socket.write(payload);
    });
    socket.on("data", (c) => {
      if (!verified) return;
      received += c.length;
      if (received > MAX_RESPONSE) return finish(reject, new NetworkError("response too large"));
      chunks.push(c);
    });
    socket.on("error", (e) => finish(reject, new NetworkError(e.code ?? "network error")));
    socket.on("close", () => {
      if (!verified) return finish(reject, new NetworkError("connection closed"));
      try {
        finish(resolve, parseResponse(Buffer.concat(chunks)));
      } catch (e) {
        finish(reject, e instanceof NetworkError ? e : new NetworkError("bad response"));
      }
    });
  });
}
