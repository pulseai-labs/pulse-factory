// The fixture server of d25: a Node `http` server on 127.0.0.1 that serves the built bundle (`PF_UI_DIR`)
// with the bridge's Content-Security-Policy and answers the bridge protocol of
// docs/contracts/ui-transport.md from `fixtures.ts` and the modules under `fixtures/`. It is test data for a rendering check: it stands in
// for the bridge's answers, and the journey lines d12, d19 and d23 keep the real controller, API and
// bridge. Its fake-ledger entry is `ui-screens-fixture-server` (channel `fake`).
//
// A route or verb with no fixture answers `bad_request` and is recorded in `unexpected`; the spec fails
// when any was recorded, naming it. A screen's state is a switch of this one server, not a second server.
import fs from "node:fs";
import http from "node:http";
import type { Socket } from "node:net";
import path from "node:path";
import {
  PAIRED,
  STATUS,
  UNREACHABLE_MESSAGE,
  agentsVerbs,
  chatVerbs,
  needsYouVerbs,
  projectsVerbs,
  settingsVerbs,
  shellVerbs,
  streamEvents,
} from "./fixtures";
import type { Verbs } from "./fixtures/verbs";

/** `paired`: every screen. `unpaired`: `paired` answers null (3a). `unreachable`: every call after `paired` fails (3c). */
export type Mode = "paired" | "unpaired" | "unreachable";

export interface FixtureServer {
  /** `http://127.0.0.1:<port>` */
  origin: string;
  setMode(mode: Mode): void;
  /** Every route or verb the page asked for that has no fixture, as `METHOD path`. */
  unexpected: string[];
  close(): Promise<void>;
}

/** The bridge's own header (`crates/pf-client/src/bridge.rs`). */
const CSP = "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'";

const TYPES: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json",
  ".woff2": "font/woff2",
  ".woff": "font/woff",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".md": "text/markdown; charset=utf-8",
  ".txt": "text/plain; charset=utf-8",
};

/**
 * The verb table: the six fixture modules' `verbs`, merged. A verb that two modules answer fails at start,
 * naming it and both modules. A verb that is not in the table is `bad_request` (unless a test overlays it).
 */
function mergeVerbs(tables: Array<[module: string, verbs: Verbs]>): Verbs {
  const merged: Verbs = {};
  const owner = new Map<string, string>();
  for (const [module, table] of tables) {
    for (const [verb, answer] of Object.entries(table)) {
      const first = owner.get(verb);
      if (first !== undefined) throw new Error(`the verb "${verb}" is answered by two fixture modules: ${first} and ${module}`);
      owner.set(verb, module);
      merged[verb] = answer;
    }
  }
  return merged;
}

const VERBS: Verbs = mergeVerbs([
  ["shell", shellVerbs],
  ["chat", chatVerbs],
  ["needs-you", needsYouVerbs],
  ["agents", agentsVerbs],
  ["settings", settingsVerbs],
  ["projects", projectsVerbs],
]);

/**
 * A per-test change of the fixture answers: a verb name to a new answer, or to a function of the base answer
 * (`(base, args) => answer`; an answer is JSON, so a function is never an answer). A test calls it before it
 * loads the page, for example one more request in `requests`, a session in `working`, or a run with a
 * `project_id`. It holds for that test only: the spec clears it in `afterEach` with `clearOverlay`. A
 * round-3 item changes another module's value only through this, inside its own `describe` block.
 */
export type Overlay = Record<string, unknown>;

let overlays: Overlay = {};

export function overlay(patch: Overlay): void {
  overlays = { ...overlays, ...patch };
}

export function clearOverlay(): void {
  overlays = {};
}

/** The answer of a verb: the table's (none when absent), then the overlay's. `undefined` means no fixture. */
function answerOf(verb: string, args: unknown): { found: boolean; value?: unknown } {
  const base = VERBS[verb];
  const patch = Object.hasOwn(overlays, verb) ? overlays[verb] : undefined;
  const has = base !== undefined || Object.hasOwn(overlays, verb);
  if (!has) return { found: false };
  const baseValue = base === undefined ? undefined : base(args);
  if (!Object.hasOwn(overlays, verb)) return { found: true, value: baseValue };
  return { found: true, value: typeof patch === "function" ? (patch as (base: unknown, args: unknown) => unknown)(baseValue, args) : patch };
}

function readBody(req: http.IncomingMessage): Promise<unknown> {
  return new Promise((resolve) => {
    const chunks: Buffer[] = [];
    req.on("data", (c: Buffer) => chunks.push(c));
    req.on("end", () => {
      try {
        resolve(chunks.length === 0 ? {} : JSON.parse(Buffer.concat(chunks).toString("utf8")));
      } catch {
        resolve({});
      }
    });
  });
}

export async function startFixtureServer(uiDir: string): Promise<FixtureServer> {
  const root = path.resolve(uiDir);
  let mode: Mode = "paired";
  const unexpected: string[] = [];
  const sockets = new Set<Socket>();

  const json = (res: http.ServerResponse, body: unknown) => {
    res.writeHead(200, { "content-type": "application/json", "cache-control": "no-store", "content-security-policy": CSP });
    res.end(JSON.stringify(body));
  };
  const ok = (res: http.ServerResponse, result: unknown) => json(res, { ok: true, result });
  const fail = (res: http.ServerResponse, kind: string, message: string) => json(res, { ok: false, error: { kind, message } });
  const unreachable = (res: http.ServerResponse) => fail(res, "unreachable", UNREACHABLE_MESSAGE);

  function stream(req: http.IncomingMessage, res: http.ServerResponse, url: URL) {
    res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-store", "content-security-policy": CSP });
    const send = (event: unknown) => res.write(`data: ${JSON.stringify(event)}\n\n`);
    if (mode === "unreachable") {
      send({ event: "closed", error: { kind: "unreachable", message: UNREACHABLE_MESSAGE } });
    } else {
      const session = url.searchParams.get("session");
      const after = Number(url.searchParams.get("after") ?? "0");
      for (const event of streamEvents(session, Number.isFinite(after) ? after : 0)) send(event);
    }
    // The stream stays open until the page closes it.
    req.on("close", () => res.end());
  }

  async function bridge(req: http.IncomingMessage, res: http.ServerResponse, url: URL) {
    const method = req.method ?? "GET";
    const route = `${method} ${url.pathname}`;
    if (route === "GET /bridge/paired") return ok(res, mode === "unpaired" ? null : PAIRED);
    if (route === "GET /bridge/stream") return stream(req, res, url);
    if (route === "POST /bridge/status") return mode === "unreachable" ? unreachable(res) : ok(res, STATUS);
    const verb = /^\/bridge\/verbs\/([a-z][a-z_]*)$/.exec(url.pathname)?.[1];
    if (method === "POST" && verb !== undefined) {
      const args = await readBody(req);
      if (mode === "unreachable") return unreachable(res);
      try {
        const answer = answerOf(verb, args);
        if (!answer.found) {
          unexpected.push(`${route}`);
          return fail(res, "bad_request", `d25 has no fixture for the verb ${verb}`);
        }
        return ok(res, answer.value);
      } catch (e) {
        unexpected.push(`${route} (${e instanceof Error ? e.message : String(e)})`);
        return fail(res, "bad_request", e instanceof Error ? e.message : "no fixture");
      }
    }
    unexpected.push(route);
    return fail(res, "bad_request", `d25 has no fixture for ${route}`);
  }

  function file(res: http.ServerResponse, url: URL) {
    const rel = decodeURIComponent(url.pathname === "/" ? "/index.html" : url.pathname);
    const target = path.resolve(root, `.${rel}`);
    if (target !== root && !target.startsWith(root + path.sep)) {
      res.writeHead(403).end();
      return;
    }
    fs.readFile(target, (err, data) => {
      if (err) {
        res.writeHead(404, { "content-type": "text/plain" }).end("not found");
        return;
      }
      res.writeHead(200, {
        "content-type": TYPES[path.extname(target)] ?? "application/octet-stream",
        "cache-control": "no-store",
        "content-security-policy": CSP,
      });
      res.end(data);
    });
  }

  const server = http.createServer((req, res) => {
    const url = new URL(req.url ?? "/", "http://127.0.0.1");
    if (url.pathname.startsWith("/bridge/")) {
      void bridge(req, res, url);
    } else if (req.method === "GET" || req.method === "HEAD") {
      file(res, url);
    } else {
      res.writeHead(405).end();
    }
  });
  server.on("connection", (socket) => {
    sockets.add(socket);
    socket.on("close", () => sockets.delete(socket));
  });
  await new Promise<void>((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  if (address === null || typeof address === "string") throw new Error("the fixture server has no address");

  return {
    origin: `http://127.0.0.1:${address.port}`,
    setMode: (next) => {
      mode = next;
    },
    unexpected,
    close: () =>
      new Promise<void>((resolve) => {
        for (const socket of sockets) socket.destroy();
        server.close(() => resolve());
      }),
  };
}
