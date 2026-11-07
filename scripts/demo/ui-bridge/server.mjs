#!/usr/bin/env node
// The test bridge: the `desktop-bridge` fake (SPINE.md "Fakes"). It stands in for the Tauri shell's
// Rust side with the same protocol (docs/contracts/ui-transport.md). It is for scripts/demo/ui-pair.sh
// and tests only. The Mac app does not use it.
//
//   node server.mjs --ui <dir> --state <dir>
//
// It serves the built bundle and the bridge protocol on 127.0.0.1 on a free port, and prints one
// line, `listening http://127.0.0.1:<port>`, once. The device token, the pin and the server address
// live in `<state>/device.json` (mode 0600) and nowhere else. It never writes the token or the code
// to stdout, stderr or an answer. No npm dependency: Node 22 only.
import fs from "node:fs";
import http from "node:http";
import path from "node:path";
import { parseArgs } from "node:util";
import { decodeCode, normalizeCode, parseAddress } from "./code.mjs";
import { NetworkError, PinError, pinnedRequest } from "./pinned-https.mjs";

const KEY_CHANGED = "server key changed: re-pair";
const MAX_BODY = 1024 * 1024 + 4096;
const HEX64 = /^[0-9a-f]{64}$/;
const VERB = /^[a-z][a-z_]{0,63}$/;

const { values: args } = parseArgs({
  options: { ui: { type: "string" }, state: { type: "string" }, port: { type: "string" } },
  strict: true,
});
if (!args.ui || !args.state) {
  process.stderr.write("usage: server.mjs --ui <dir> --state <dir>\n");
  process.exit(2);
}
const uiRoot = path.resolve(args.ui);
const stateDir = path.resolve(args.state);
const deviceFile = path.join(stateDir, "device.json");
if (!fs.existsSync(path.join(uiRoot, "index.html"))) {
  process.stderr.write("the --ui directory has no index.html\n");
  process.exit(2);
}

// ---- answers ------------------------------------------------------------------------------------

class Failure extends Error {
  constructor(kind, message) {
    super(message);
    this.kind = kind;
  }
}
const ok = (result) => ({ ok: true, result });
const bad = (kind, message) => ({ ok: false, error: { kind, message } });

/** The server's own text, bounded, and never one that holds a secret. */
function serverText(json, status, secrets) {
  const raw = typeof json?.error === "string" ? json.error : "";
  const text = raw.replace(/[\u0000-\u001f\u007f]/g, " ").trim().slice(0, 300);
  if (text === "" || secrets.some((s) => s && text.includes(s))) return `the server answered ${status}`;
  return text;
}

function parseJson(buf) {
  try {
    return JSON.parse(buf.toString("utf8"));
  } catch {
    return undefined;
  }
}

/** Turn an API answer into a result, or throw the kind that docs/contracts/ui-transport.md names. */
function result(res, route, secrets) {
  const json = parseJson(res.body);
  if (res.status === 200) {
    if (json === undefined || json === null) throw new Failure("server", "the server's answer is not JSON");
    return json;
  }
  const text = serverText(json, res.status, secrets);
  switch (res.status) {
    case 401:
      throw new Failure(route === "pair" ? "code_refused" : "unauthorized", text);
    case 403:
      throw new Failure("forbidden", text);
    case 422:
      throw new Failure("refused", text);
    case 429:
      throw new Failure("rate_limited", text);
    case 400:
    case 413:
      throw new Failure("bad_request", text);
    default:
      throw new Failure("server", text);
  }
}

async function call(address, pinHex, opts) {
  const target = parseAddress(address);
  if (!target) throw new Failure("server", "the stored server address is not valid");
  try {
    return await pinnedRequest({ ...target, pinHex, ...opts });
  } catch (e) {
    if (e instanceof PinError) throw new Failure("key_changed", KEY_CHANGED);
    if (e instanceof NetworkError) throw new Failure("unreachable", `cannot reach ${address}`);
    throw new Failure("server", "the bridge failed");
  }
}

// ---- the device file ----------------------------------------------------------------------------

function loadDevice() {
  let text;
  try {
    text = fs.readFileSync(deviceFile, "utf8");
  } catch (e) {
    if (e.code === "ENOENT") return null;
    throw new Failure("server", "device.json cannot be read");
  }
  const d = parseJson(Buffer.from(text));
  if (
    !d ||
    typeof d.address !== "string" ||
    !HEX64.test(d.pin ?? "") ||
    !HEX64.test(d.token ?? "") ||
    typeof d.device?.id !== "string"
  ) {
    throw new Failure("server", "device.json is not valid");
  }
  return d;
}

function saveDevice(d) {
  fs.mkdirSync(stateDir, { recursive: true, mode: 0o700 });
  const tmp = `${deviceFile}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(d), { mode: 0o600 });
  fs.chmodSync(tmp, 0o600);
  fs.renameSync(tmp, deviceFile);
}

// ---- the protocol -------------------------------------------------------------------------------

function paired() {
  const d = loadDevice();
  return ok(d ? { address: d.address, device: d.device } : null);
}

async function pair(body) {
  if (typeof body?.code !== "string" || typeof body?.device_name !== "string") {
    throw new Failure("bad_request", "pair needs a code and a device name");
  }
  const code = normalizeCode(body.code);
  const decoded = decodeCode(code);
  if (!decoded) throw new Failure("code_refused", "the code is not accepted");
  // The pin is checked before one byte is sent. A mismatch is `key_changed`.
  const res = await call(decoded.address, decoded.spkiHex, {
    method: "POST",
    path: "/v1/pair",
    body: JSON.stringify({ code, device_name: body.device_name }),
  });
  const out = result(res, "pair", [code]);
  if (!HEX64.test(out?.token ?? "") || typeof out?.device?.id !== "string") {
    throw new Failure("server", "the server's pair answer is not the documented one");
  }
  saveDevice({ address: decoded.address, pin: decoded.spkiHex, token: out.token, device: out.device });
  return ok({ address: decoded.address, device: out.device });
}

async function tokenCall(method, apiPath, body) {
  const d = loadDevice();
  if (!d) throw new Failure("unauthorized", "this device is not paired");
  const res = await call(d.address, d.pin, {
    method,
    path: apiPath,
    headers: { Authorization: `Bearer ${d.token}` },
    body: method === "POST" ? JSON.stringify(body ?? {}) : undefined,
  });
  return ok(result(res, "token", [d.token]));
}

function forget() {
  fs.rmSync(deviceFile, { force: true });
  return ok(null);
}

async function dispatch(method, pathname, body) {
  if (pathname === "/bridge/paired" && method === "GET") return paired();
  if (method !== "POST") throw new Failure("bad_request", "the bridge route needs POST");
  if (pathname === "/bridge/pair") return pair(body);
  if (pathname === "/bridge/status") return tokenCall("GET", "/v1/status");
  if (pathname === "/bridge/forget") return forget();
  if (pathname.startsWith("/bridge/verbs/")) {
    const verb = pathname.slice("/bridge/verbs/".length);
    if (!VERB.test(verb)) throw new Failure("bad_request", "the verb name is not valid");
    if (body === null || typeof body !== "object" || Array.isArray(body)) {
      throw new Failure("bad_request", "the verb arguments must be a JSON object");
    }
    return tokenCall("POST", `/v1/verbs/${verb}`, body);
  }
  throw new Failure("bad_request", "unknown bridge route");
}

// ---- HTTP ---------------------------------------------------------------------------------------

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".ico": "image/x-icon",
  ".woff2": "font/woff2",
  ".woff": "font/woff",
  ".map": "application/json",
};
const SECURITY = {
  "cache-control": "no-store",
  "x-content-type-options": "nosniff",
  // The bundle fetches nothing but its own origin.
  "content-security-policy": "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'",
};

function send(res, status, type, body) {
  res.writeHead(status, { ...SECURITY, "content-type": type });
  res.end(body);
}

function serveFile(res, pathname) {
  let rel;
  try {
    rel = decodeURIComponent(pathname);
  } catch {
    return send(res, 400, "text/plain", "bad path");
  }
  if (rel.includes("\0")) return send(res, 400, "text/plain", "bad path");
  let file = path.join(uiRoot, rel);
  if (file !== uiRoot && !file.startsWith(uiRoot + path.sep)) return send(res, 403, "text/plain", "forbidden");
  try {
    if (fs.statSync(file).isDirectory()) file = path.join(file, "index.html");
    const data = fs.readFileSync(file);
    send(res, 200, TYPES[path.extname(file).toLowerCase()] ?? "application/octet-stream", data);
  } catch {
    send(res, 404, "text/plain", "not found");
  }
}

function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    req.on("data", (c) => {
      size += c.length;
      if (size > MAX_BODY) {
        reject(new Failure("bad_request", "the body is over 1 MiB"));
        req.destroy();
        return;
      }
      chunks.push(c);
    });
    req.on("end", () => resolve(Buffer.concat(chunks)));
    req.on("error", () => reject(new Failure("bad_request", "the body could not be read")));
  });
}

const server = http.createServer(async (req, res) => {
  const own = `127.0.0.1:${server.address().port}`;
  // Only the bundle's own page may call the bridge: no foreign Host (DNS rebinding), no foreign
  // Origin, and a JSON content type on every POST (a cross-site form cannot send one).
  if (req.headers.host !== own) return send(res, 403, "text/plain", "forbidden");
  if (req.headers.origin !== undefined && req.headers.origin !== `http://${own}`) {
    return send(res, 403, "text/plain", "forbidden");
  }
  const pathname = new URL(req.url ?? "/", `http://${own}`).pathname;
  const isBridge = pathname.startsWith("/bridge/");
  if (!isBridge) {
    if (req.method !== "GET" && req.method !== "HEAD") return send(res, 405, "text/plain", "method not allowed");
    return serveFile(res, pathname);
  }
  if (req.method === "POST" && !/^application\/json\b/i.test(req.headers["content-type"] ?? "")) {
    return send(res, 403, "text/plain", "forbidden");
  }
  let answer;
  try {
    let body;
    if (req.method === "POST") {
      const raw = await readBody(req);
      body = raw.length === 0 ? {} : parseJson(raw);
      if (body === undefined) throw new Failure("bad_request", "the body is not JSON");
    }
    // The raw path, not the decoded one: `..%2F` must not turn into a path.
    answer = await dispatch(req.method, req.url.split("?")[0], body);
  } catch (e) {
    answer = e instanceof Failure ? bad(e.kind, e.message) : bad("server", "the bridge failed");
  }
  send(res, 200, "application/json", JSON.stringify(answer));
});

server.listen(args.port ? Number(args.port) : 0, "127.0.0.1", () => {
  process.stdout.write(`listening http://127.0.0.1:${server.address().port}\n`);
});

for (const sig of ["SIGTERM", "SIGINT"]) {
  process.on(sig, () => {
    server.close();
    process.exit(0);
  });
}
