// The test bridge (`desktop-bridge` fake): `node --test scripts/demo/ui-bridge/test/bridge.test.mjs`.
// A local TLS server with a throwaway self-signed key (made by openssl in a temp directory) stands in
// for the controller's API. The bridge runs as a child process, as d12 runs it.
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import https from "node:https";
import os from "node:os";
import path from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const serverScript = path.join(here, "..", "server.mjs");

const TOKEN = "ab".repeat(32); // 64 hex digits, like a device token
const DEVICE = {
  id: "D-1",
  name: "d12 test",
  paired_at: "2026-11-07T10:00:00Z",
  last_seen_at: null,
  last_peer: null,
  revoked_at: null,
};
const STATUS = {
  schema_version: 1,
  version: "0.0.0-test",
  key_spki_sha256: "0".repeat(64),
  device: DEVICE,
};

let tmp;
let uiDir;
let keyA;
let keyB;
const children = [];
const apis = []; // every fake API, so a failed test cannot leave one listening
const printed = []; // everything any bridge wrote to stdout or stderr
const secrets = new Set([TOKEN]);

function makeKey(name) {
  const key = path.join(tmp, `${name}.key`);
  const cert = path.join(tmp, `${name}.crt`);
  execFileSync(
    "openssl",
    ["req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1", "-nodes", "-keyout", key, "-out", cert, "-subj", `/CN=${name}`, "-days", "2"],
    { stdio: "ignore" },
  );
  // The SPKI hash by openssl, not by the code under test.
  const pubkey = execFileSync("openssl", ["x509", "-in", cert, "-pubkey", "-noout"], { encoding: "utf8" });
  const spki = execFileSync("openssl", ["pkey", "-pubin", "-outform", "DER"], { input: pubkey });
  return {
    key: fs.readFileSync(key),
    cert: fs.readFileSync(cert),
    spki: crypto.createHash("sha256").update(spki).digest(),
  };
}

before(() => {
  tmp = fs.mkdtempSync(path.join(os.tmpdir(), "pf-bridge-test-"));
  uiDir = path.join(tmp, "ui");
  fs.mkdirSync(path.join(uiDir, "assets"), { recursive: true });
  fs.writeFileSync(path.join(uiDir, "index.html"), "<!doctype html><title>pf</title>");
  fs.writeFileSync(path.join(uiDir, "assets", "app.js"), "console.log(1)");
  fs.writeFileSync(path.join(tmp, "secret.txt"), "outside the bundle");
  keyA = makeKey("a");
  keyB = makeKey("b");
});

after(async () => {
  for (const c of children) c.kill();
  await Promise.all(apis.map((a) => a.stop()));
  fs.rmSync(tmp, { recursive: true, force: true });
});

// ---- a fake API behind TLS 1.3 ------------------------------------------------------------------

async function startApi(keys, port = 0) {
  const state = {
    bytes: 0, // application bytes the server read after the handshake
    connections: 0,
    requests: [],
    respond: () => ({ status: 200, json: {} }),
  };
  const server = https.createServer({ key: keys.key, cert: keys.cert, minVersion: "TLSv1.3" }, (req, res) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => {
      const body = Buffer.concat(chunks).toString("utf8");
      state.requests.push({ method: req.method, url: req.url, headers: req.headers, body });
      const r = state.respond(req, body);
      res.writeHead(r.status, { "content-type": "application/json", "cache-control": "no-store" });
      res.end(r.raw ?? JSON.stringify(r.json));
    });
  });
  server.on("secureConnection", (socket) => {
    state.connections += 1;
    socket.on("data", (c) => {
      state.bytes += c.length;
    });
    socket.on("error", () => {});
  });
  await new Promise((resolve) => server.listen(port, "127.0.0.1", resolve));
  const api = {
    state,
    port: server.address().port,
    stop: () =>
      new Promise((resolve) => {
        server.close(() => resolve());
        server.closeAllConnections();
      }),
  };
  apis.push(api);
  return api;
}

function okApi(api) {
  api.state.respond = (req) => {
    if (req.url === "/v1/pair") return { status: 200, json: { schema_version: 1, device: DEVICE, token: TOKEN } };
    if (req.url === "/v1/status") return { status: 200, json: STATUS };
    if (req.url.startsWith("/v1/verbs/")) return { status: 200, json: { schema_version: 1, projects: [] } };
    return { status: 404, json: { error: "not found" } };
  };
}

// ---- the bridge as a child process --------------------------------------------------------------

async function startBridge() {
  const stateDir = path.join(tmp, `state-${children.length}`);
  const child = spawn(process.execPath, [serverScript, "--ui", uiDir, "--state", stateDir], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  children.push(child);
  const out = [];
  for (const stream of [child.stdout, child.stderr]) {
    stream.on("data", (c) => {
      out.push(c.toString("utf8"));
      printed.push(c.toString("utf8"));
    });
  }
  const port = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("the bridge did not print its listening line")), 10000);
    const check = () => {
      const m = /listening http:\/\/127\.0\.0\.1:(\d+)/.exec(out.join(""));
      if (m) {
        clearTimeout(timer);
        resolve(Number(m[1]));
      }
    };
    child.stdout.on("data", check);
    child.on("exit", () => reject(new Error(`the bridge exited early: ${out.join("")}`)));
  });
  const base = `http://127.0.0.1:${port}`;
  return {
    port,
    stateDir,
    deviceFile: path.join(stateDir, "device.json"),
    async call(method, route, body) {
      const res = await fetch(base + route, {
        method,
        headers: method === "POST" ? { "content-type": "application/json" } : undefined,
        body: method === "POST" ? JSON.stringify(body ?? {}) : undefined,
      });
      const text = await res.text();
      let json;
      try {
        json = JSON.parse(text);
      } catch {
        json = undefined;
      }
      return { status: res.status, text, json };
    },
  };
}

const B32 = "abcdefghijklmnopqrstuvwxyz234567";
function base32(buf) {
  let bits = 0;
  let acc = 0;
  let out = "";
  for (const b of buf) {
    acc = (acc << 8) | b;
    bits += 8;
    while (bits >= 5) {
      out += B32[(acc >> (bits - 5)) & 31];
      bits -= 5;
    }
    acc &= (1 << bits) - 1;
  }
  if (bits > 0) out += B32[(acc << (5 - bits)) & 31];
  return out;
}

function makeCode(hash, address, secret = crypto.randomBytes(10)) {
  const code = `pf1-${base32(Buffer.concat([Buffer.from([1]), secret, hash, Buffer.from(address)]))}`;
  secrets.add(code);
  secrets.add(secret.toString("hex"));
  return code;
}

async function pairedBridge(api, keys = keyA) {
  const bridge = await startBridge();
  okApi(api);
  const code = makeCode(keys.spki, `127.0.0.1:${api.port}`);
  const res = await bridge.call("POST", "/bridge/pair", { code, device_name: "d12 test" });
  assert.equal(res.json?.ok, true, res.text);
  return { bridge, code };
}

// ---- the tests ---------------------------------------------------------------------------------

test("pair with the server's own hash pairs, keeps the token in device.json (0600) and answers with no token", async () => {
  const api = await startApi(keyA);
  try {
    okApi(api);
    const bridge = await startBridge();
    const address = `127.0.0.1:${api.port}`;
    const code = makeCode(keyA.spki, address);
    const res = await bridge.call("POST", "/bridge/pair", { code: `  ${code.toUpperCase()}\n`, device_name: "d12 test" });

    assert.equal(res.status, 200);
    assert.deepEqual(res.json, { ok: true, result: { address, device: DEVICE } });
    assert.ok(!res.text.includes(TOKEN), "the answer holds the token");
    assert.ok(!res.text.toLowerCase().includes(code.slice(4)), "the answer holds the code");

    // The server saw one POST /v1/pair with the normalised code and the name.
    assert.equal(api.state.requests.length, 1);
    const req = api.state.requests[0];
    assert.equal(req.method, "POST");
    assert.equal(req.url, "/v1/pair");
    assert.equal(req.headers["content-type"], "application/json");
    assert.deepEqual(JSON.parse(req.body), { code, device_name: "d12 test" });
    assert.equal(req.headers.authorization, undefined);

    // The token, the pin and the address are in one file of mode 0600.
    const mode = fs.statSync(bridge.deviceFile).mode & 0o777;
    assert.equal(mode, 0o600);
    const kept = JSON.parse(fs.readFileSync(bridge.deviceFile, "utf8"));
    assert.equal(kept.token, TOKEN);
    assert.equal(kept.address, address);
    assert.equal(kept.pin, keyA.spki.toString("hex"));
    assert.deepEqual(fs.readdirSync(bridge.stateDir), ["device.json"]);

    // `paired` is answered from the file, with no token and no network call.
    const before = api.state.requests.length;
    const paired = await bridge.call("GET", "/bridge/paired");
    assert.deepEqual(paired.json, { ok: true, result: { address, device: DEVICE } });
    assert.ok(!paired.text.includes(TOKEN));
    assert.equal(api.state.requests.length, before);

    // A token call carries the bearer token. The answer to the page does not.
    const status = await bridge.call("POST", "/bridge/status", {});
    assert.deepEqual(status.json, { ok: true, result: STATUS });
    const sreq = api.state.requests.at(-1);
    assert.equal(sreq.url, "/v1/status");
    assert.equal(sreq.headers.authorization, `Bearer ${TOKEN}`);

    const verb = await bridge.call("POST", "/bridge/verbs/project_list", {});
    assert.deepEqual(verb.json, { ok: true, result: { schema_version: 1, projects: [] } });
    const vreq = api.state.requests.at(-1);
    assert.equal(vreq.method, "POST");
    assert.equal(vreq.url, "/v1/verbs/project_list");
    assert.equal(vreq.headers.authorization, `Bearer ${TOKEN}`);
    assert.ok(!verb.text.includes(TOKEN));
  } finally {
    await api.stop();
  }
});

test("a code whose hash is another key's answers key_changed, and the server gets zero bytes after the handshake", async () => {
  const api = await startApi(keyA);
  try {
    okApi(api);
    const bridge = await startBridge();
    const code = makeCode(keyB.spki, `127.0.0.1:${api.port}`);
    const res = await bridge.call("POST", "/bridge/pair", { code, device_name: "d12 test" });

    assert.deepEqual(res.json, { ok: false, error: { kind: "key_changed", message: "server key changed: re-pair" } });
    // The `key_changed` answer shows that the bridge connected and read the key. It then closed the
    // socket at once: the server may or may not finish the handshake, but it reads no byte.
    assert.equal(api.state.bytes, 0);
    assert.equal(api.state.requests.length, 0);
    assert.equal(fs.existsSync(bridge.deviceFile), false);
  } finally {
    await api.stop();
  }
});

test("after a pair, the server's key changes: the next call answers key_changed and sends nothing", async () => {
  const first = await startApi(keyA);
  const { bridge } = await pairedBridge(first);
  const port = first.port;
  await first.stop();
  const second = await startApi(keyB, port);
  try {
    okApi(second);
    for (const [route, body] of [
      ["/bridge/status", {}],
      ["/bridge/verbs/project_list", {}],
    ]) {
      const res = await bridge.call("POST", route, body);
      assert.deepEqual(res.json, { ok: false, error: { kind: "key_changed", message: "server key changed: re-pair" } });
    }
    assert.equal(second.state.bytes, 0);
    assert.equal(second.state.requests.length, 0);
    // The pin stays: only a new pairing accepts a new key.
    assert.ok(fs.existsSync(bridge.deviceFile));
  } finally {
    await second.stop();
  }
});

test("the API's answers map to the transport's error kinds, and a closed port is unreachable", async () => {
  const api = await startApi(keyA);
  const { bridge } = await pairedBridge(api);
  try {
    const cases = [
      [401, { error: "not authorized" }, "unauthorized", "not authorized"],
      [403, { error: 'the verb "x" is not open to devices' }, "forbidden", 'the verb "x" is not open to devices'],
      [422, { error: "the root does not exist: /nope" }, "refused", "the root does not exist: /nope"],
      [429, { error: "too many failed calls; wait a minute" }, "rate_limited", "too many failed calls; wait a minute"],
      [413, { error: "the body is over 1 MiB" }, "bad_request", "the body is over 1 MiB"],
      [400, { error: "the body is not JSON" }, "bad_request", "the body is not JSON"],
      [500, { error: "the controller failed" }, "server", "the controller failed"],
    ];
    for (const [status, json, kind, message] of cases) {
      api.state.respond = () => ({ status, json });
      const res = await bridge.call("POST", "/bridge/verbs/project_list", {});
      assert.equal(res.status, 200, "every bridge answer is 200");
      assert.deepEqual(res.json, { ok: false, error: { kind, message } }, `HTTP ${status}`);
    }
    // The same 401 on /bridge/status is `unauthorized` too.
    api.state.respond = () => ({ status: 401, json: { error: "not authorized" } });
    assert.equal((await bridge.call("POST", "/bridge/status", {})).json.error.kind, "unauthorized");

    // An answer that is not the documented JSON is `server`.
    api.state.respond = () => ({ status: 200, raw: "<html>not json</html>" });
    assert.equal((await bridge.call("POST", "/bridge/status", {})).json.error.kind, "server");

    // A closed port: no HTTP answer.
    await api.stop();
    const closed = await bridge.call("POST", "/bridge/status", {});
    assert.deepEqual(closed.json.ok, false);
    assert.equal(closed.json.error.kind, "unreachable");
  } finally {
    await api.stop().catch(() => {});
  }
});

test("a 401 on /v1/pair is code_refused, and a closed port on pair is unreachable", async () => {
  const api = await startApi(keyA);
  try {
    const bridge = await startBridge();
    api.state.respond = () => ({ status: 401, json: { error: "the code is not accepted" } });
    const code = makeCode(keyA.spki, `127.0.0.1:${api.port}`);
    const res = await bridge.call("POST", "/bridge/pair", { code, device_name: "d12 test" });
    assert.deepEqual(res.json, { ok: false, error: { kind: "code_refused", message: "the code is not accepted" } });
    assert.equal(fs.existsSync(bridge.deviceFile), false);

    api.state.respond = () => ({ status: 422, json: { error: "the device name is too long" } });
    const named = await bridge.call("POST", "/bridge/pair", { code, device_name: "x" });
    assert.deepEqual(named.json.error, { kind: "refused", message: "the device name is too long" });

    await api.stop();
    const closed = await bridge.call("POST", "/bridge/pair", { code, device_name: "d12 test" });
    assert.equal(closed.json.error.kind, "unreachable");
  } finally {
    await api.stop().catch(() => {});
  }
});

test("a malformed code is code_refused with no network call", async () => {
  const bridge = await startBridge();
  for (const code of ["hello", "pf1-", "pf1-!!!!", `pf1-${base32(Buffer.from([2, 1, 2, 3]))}`, `pf1-${base32(Buffer.from([1, 1, 2, 3]))}`]) {
    const res = await bridge.call("POST", "/bridge/pair", { code, device_name: "d12 test" });
    assert.equal(res.json.ok, false, code);
    assert.equal(res.json.error.kind, "code_refused", code);
  }
  assert.equal(fs.existsSync(bridge.deviceFile), false);
});

test("forget removes device.json, and paired then answers no server", async () => {
  const api = await startApi(keyA);
  const { bridge } = await pairedBridge(api);
  try {
    assert.ok(fs.existsSync(bridge.deviceFile));
    const res = await bridge.call("POST", "/bridge/forget", {});
    assert.deepEqual(res.json, { ok: true, result: null });
    assert.equal(fs.existsSync(bridge.deviceFile), false);
    assert.deepEqual((await bridge.call("GET", "/bridge/paired")).json, { ok: true, result: null });
    // A token call with no device is refused at the bridge, and sends nothing.
    const before = api.state.requests.length;
    const status = await bridge.call("POST", "/bridge/status", {});
    assert.equal(status.json.error.kind, "unauthorized");
    assert.equal(api.state.requests.length, before);
    // Forgetting twice is fine.
    assert.deepEqual((await bridge.call("POST", "/bridge/forget", {})).json, { ok: true, result: null });
  } finally {
    await api.stop();
  }
});

test("a verb name is checked before it reaches a path", async () => {
  const api = await startApi(keyA);
  const { bridge } = await pairedBridge(api);
  try {
    const before = api.state.requests.length;
    const res = await bridge.call("POST", "/bridge/verbs/..%2Fpair", {});
    assert.equal(res.json.ok, false);
    assert.equal(res.json.error.kind, "bad_request");
    assert.equal(api.state.requests.length, before);
  } finally {
    await api.stop();
  }
});

function raw(port, { method = "GET", pathname = "/", headers = {}, body } = {}) {
  return new Promise((resolve, reject) => {
    const req = http.request({ host: "127.0.0.1", port, method, path: pathname, headers }, (res) => {
      const chunks = [];
      res.on("data", (c) => chunks.push(c));
      res.on("end", () => resolve({ status: res.statusCode, headers: res.headers, text: Buffer.concat(chunks).toString("utf8") }));
    });
    req.on("error", reject);
    req.end(body);
  });
}

test("the bridge serves the bundle's files and nothing outside them", async () => {
  const bridge = await startBridge();
  const index = await raw(bridge.port, { pathname: "/" });
  assert.equal(index.status, 200);
  assert.match(index.headers["content-type"], /text\/html/);
  assert.match(index.text, /<title>pf<\/title>/);
  const js = await raw(bridge.port, { pathname: "/assets/app.js" });
  assert.equal(js.status, 200);
  assert.match(js.headers["content-type"], /javascript/);
  assert.equal((await raw(bridge.port, { pathname: "/nope.txt" })).status, 404);
  for (const pathname of ["/%2e%2e/secret.txt", "/assets/..%2f..%2fsecret.txt", "/../secret.txt"]) {
    const res = await raw(bridge.port, { pathname });
    assert.ok([400, 403, 404].includes(res.status), `${pathname} -> ${res.status}`);
    assert.ok(!res.text.includes("outside the bundle"), pathname);
  }
});

test("the bridge refuses a foreign Host, a foreign Origin and a POST that is not JSON", async () => {
  const bridge = await startBridge();
  const own = `127.0.0.1:${bridge.port}`;
  const good = { host: own, "content-type": "application/json" };
  assert.equal((await raw(bridge.port, { method: "GET", pathname: "/bridge/paired", headers: { host: own } })).status, 200);
  assert.equal((await raw(bridge.port, { method: "GET", pathname: "/bridge/paired", headers: { host: "rebind.example" } })).status, 403);
  assert.equal(
    (await raw(bridge.port, { method: "POST", pathname: "/bridge/forget", headers: { ...good, origin: "https://evil.example" }, body: "{}" })).status,
    403,
  );
  assert.equal(
    (await raw(bridge.port, { method: "POST", pathname: "/bridge/forget", headers: { host: own, "content-type": "text/plain" }, body: "{}" })).status,
    403,
  );
  assert.equal(
    (await raw(bridge.port, { method: "POST", pathname: "/bridge/forget", headers: { ...good, origin: `http://${own}` }, body: "{}" })).status,
    200,
  );
});

test("nothing the bridge prints holds the token or the code", async () => {
  assert.ok(printed.length > 0, "no bridge output was captured");
  const all = printed.join("");
  for (const secret of secrets) {
    assert.ok(!all.includes(secret), "a secret is in the bridge's output");
    assert.ok(!all.toLowerCase().includes(secret.toLowerCase()), "a secret is in the bridge's output");
  }
  const lines = all.split("\n").filter((l) => l !== "");
  assert.ok(lines.length > 0);
  for (const line of lines) {
    assert.match(line, /^listening http:\/\/127\.0\.0\.1:\d+$/, `unexpected bridge output: ${line}`);
  }
});
