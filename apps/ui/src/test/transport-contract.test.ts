// One table of cases, run against both implementations of the transport interface
// (docs/contracts/ui-transport.md): the bridge transport (`fetch` and an `EventSource` factory
// mocked) and the Tauri transport (`invoke` and `Channel` mocked). Each new method, each error kind
// and the stream behave the same in both, so the screens are the same code in both.
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createBridgeTransport } from "../transport/bridge";
import { createTauriTransport } from "../transport/tauri";
import {
  TRANSPORT_ERROR_KINDS,
  TransportError,
  type StreamEvent,
  type Transport,
  type TransportErrorKind,
} from "../transport/types";

/** An `EventSource` that the test drives. */
class FakeEventSource {
  static all: FakeEventSource[] = [];
  onmessage: ((event: { data: unknown }) => void) | null = null;
  onerror: ((event: unknown) => void) | null = null;
  closed = false;
  constructor(readonly url: string) {
    FakeEventSource.all.push(this);
  }
  close(): void {
    this.closed = true;
  }
  /** The bridge sends each native event as one `data:` line of JSON. */
  deliver(native: unknown): void {
    this.onmessage?.({ data: JSON.stringify(native) });
  }
  deliverRaw(data: unknown): void {
    this.onmessage?.({ data });
  }
}

/** A Tauri `Channel` that the test drives. */
class FakeChannel {
  static all: FakeChannel[] = [];
  onmessage: (message: unknown) => void = () => {};
  constructor() {
    FakeChannel.all.push(this);
  }
}

/** One implementation of the interface, with the native side behind it. */
interface Rig {
  name: "bridge" | "tauri";
  transport: Transport;
  /** The native side answers the next call with this result. */
  answerNext(result: unknown): void;
  /** The native side fails the next call, with a refusal word when it has one (r1.s4.w3, W3). */
  failNext(kind: string, message: string, refusal?: string): void;
  /** What the last call sent natively: where, and with what. */
  lastCall(): { target: string; payload: unknown };
  /** Every native call, as JSON text, for the no-token scan. */
  allCalls(): string;
  /** The native side sends one event of an open stream. */
  nativeEvent(native: unknown): void;
  /** The native side sends a value that is not an event of the contract. */
  nativeGarbage(value: unknown): void;
  /** Whether the native stream was closed by the transport. */
  nativeClosed(): boolean;
  /** The native side fails to open the stream (the Tauri side only). */
  failOpen(kind: string, message: string): void;
}

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), { status: 200, headers: { "content-type": "application/json" } });
}

function bridgeRig(): Rig {
  const fetchMock = vi.fn();
  FakeEventSource.all = [];
  const transport = createBridgeTransport(
    (input, init) => fetchMock(input, init) as Promise<Response>,
    (url) => new FakeEventSource(url),
  );
  return {
    name: "bridge",
    transport,
    answerNext: (result) => fetchMock.mockResolvedValueOnce(jsonResponse({ ok: true, result })),
    failNext: (kind, message, refusal) =>
      fetchMock.mockResolvedValueOnce(
        jsonResponse({ ok: false, error: refusal === undefined ? { kind, message } : { kind, message, refusal } }),
      ),
    lastCall: () => {
      const [target, init] = fetchMock.mock.calls.at(-1) as [string, RequestInit | undefined];
      const body = init?.body;
      return { target, payload: typeof body === "string" ? (JSON.parse(body) as unknown) : undefined };
    },
    allCalls: () => JSON.stringify(fetchMock.mock.calls) + JSON.stringify(FakeEventSource.all.map((e) => e.url)),
    nativeEvent: (native) => FakeEventSource.all.at(-1)?.deliver(native),
    nativeGarbage: (value) => FakeEventSource.all.at(-1)?.deliverRaw(value),
    nativeClosed: () => FakeEventSource.all.at(-1)?.closed ?? false,
    failOpen: () => {
      // The bridge never fails an open: a stream that cannot open is an `EventSource` error.
      throw new Error("the bridge has no failed open");
    },
  };
}

function tauriRig(): Rig {
  const invoke = vi.fn();
  FakeChannel.all = [];
  const transport = createTauriTransport(
    (command, args) => invoke(command, args) as Promise<unknown>,
    () => new FakeChannel(),
  );
  return {
    name: "tauri",
    transport,
    answerNext: (result) => invoke.mockResolvedValueOnce(result),
    failNext: (kind, message, refusal) =>
      invoke.mockRejectedValueOnce(refusal === undefined ? { kind, message } : { kind, message, refusal }),
    lastCall: () => {
      const [target, payload] = invoke.mock.calls.at(-1) as [string, unknown];
      return { target, payload };
    },
    allCalls: () => JSON.stringify(invoke.mock.calls.map(([c, a]) => [c, a === undefined ? null : stripChannels(a)])),
    nativeEvent: (native) => FakeChannel.all.at(-1)?.onmessage(native),
    nativeGarbage: (value) => FakeChannel.all.at(-1)?.onmessage(value),
    nativeClosed: () => invoke.mock.calls.some(([c]) => c === "close_stream"),
    failOpen: (kind, message) => {
      invoke.mockRejectedValueOnce({ kind, message });
    },
  };
}

/** A channel is not JSON: show it as its class. */
function stripChannels(args: unknown): unknown {
  return JSON.parse(JSON.stringify(args, (_k, v: unknown) => (v instanceof FakeChannel ? "<channel>" : v))) as unknown;
}

const RIGS: Array<[string, () => Rig]> = [
  ["bridge", bridgeRig],
  ["tauri", tauriRig],
];

/**
 * Where a verb goes in each implementation. A verb added since `r1.s4` (`viaVerb`) goes through the
 * one `verb` command of the Mac app, with its name and arguments; the bridge posts it to its path as
 * every other verb (r1.s4.w3, W5).
 */
function target(rig: Rig, c: { verb: string; viaVerb?: boolean }): string {
  if (rig.name === "bridge") return `/bridge/verbs/${c.verb}`;
  return c.viaVerb ? "verb" : c.verb;
}

function payload(rig: Rig, c: { verb: string; args: unknown; viaVerb?: boolean }): unknown {
  if (rig.name === "bridge") return c.args;
  return c.viaVerb ? { name: c.verb, args: c.args } : { args: c.args };
}

const SESSION = { id: "S-1" };
const CASES: Array<{
  method: string;
  verb: string;
  /** True for a verb the Mac app reaches through its `verb` command. */
  viaVerb?: boolean;
  args: unknown;
  run: (t: Transport) => Promise<unknown>;
  result: unknown;
}> = [
  { method: "sessionList", verb: "session_list", args: { project_id: "P-1" }, run: (t) => t.sessionList({ project_id: "P-1" }), result: { sessions: [SESSION] } },
  { method: "sessionList (no arguments)", verb: "session_list", args: {}, run: (t) => t.sessionList(), result: { sessions: [] } },
  {
    method: "sessionStart",
    verb: "session_start",
    args: { project_id: "P-1", agent_id: "orchestrator", command_id: "c-1" },
    run: (t) => t.sessionStart({ project_id: "P-1", agent_id: "orchestrator", command_id: "c-1" }),
    result: { replayed: false, session_id: "S-1", worktree_id: "W-1", worktree_dir: "/work", note: null },
  },
  {
    method: "sessionSend",
    verb: "session_send",
    args: { session_id: "S-1", text: "hi", command_id: "c-2" },
    run: (t) => t.sessionSend({ session_id: "S-1", text: "hi", command_id: "c-2" }),
    result: { replayed: false, session: SESSION, note: null },
  },
  {
    method: "sessionAnswer",
    verb: "session_answer",
    args: { session_id: "S-1", seq: 4, answer: { type: "allow" }, command_id: "c-3" },
    run: (t) => t.sessionAnswer({ session_id: "S-1", seq: 4, answer: { type: "allow" }, command_id: "c-3" }),
    result: { replayed: false, session: SESSION, note: null },
  },
  {
    method: "sessionInterrupt",
    verb: "session_interrupt",
    args: { session_id: "S-1", command_id: "c-4" },
    run: (t) => t.sessionInterrupt({ session_id: "S-1", command_id: "c-4" }),
    result: { replayed: false, session: SESSION, note: null },
  },
  {
    method: "sessionSet",
    verb: "session_set",
    args: { session_id: "S-1", effort: "medium", command_id: "c-5" },
    run: (t) => t.sessionSet({ session_id: "S-1", effort: "medium", command_id: "c-5" }),
    result: { replayed: false, session: SESSION, note: null },
  },
  {
    method: "sessionEnd",
    verb: "session_end",
    args: { session_id: "S-1", command_id: "c-6" },
    run: (t) => t.sessionEnd({ session_id: "S-1", command_id: "c-6" }),
    result: { replayed: false, session_id: "S-1", note: null },
  },
  // r1.s4.w3: the device verbs, the settings verbs and `run_list`.
  {
    method: "requests",
    verb: "requests",
    viaVerb: true,
    args: { all: true },
    run: (t) => t.requests({ all: true }),
    result: { schema_version: 1, open: 0, requests: [], claim: null },
  },
  {
    method: "requests (no arguments)",
    verb: "requests",
    viaVerb: true,
    args: {},
    run: (t) => t.requests(),
    result: { schema_version: 1, open: 0, requests: [], claim: null },
  },
  {
    method: "show",
    verb: "show",
    viaVerb: true,
    args: { id: "R-1" },
    run: (t) => t.show({ id: "R-1" }),
    result: { schema_version: 1, id: "R-1", body: { stop_text: "which?" }, answer: null },
  },
  {
    method: "confirmCode",
    verb: "confirm_code",
    viaVerb: true,
    args: { subject: { kind: "request", request: "R-1", revision: 1, verb: "ack" }, command_id: "c-7" },
    run: (t) => t.confirmCode({ subject: { kind: "request", request: "R-1", revision: 1, verb: "ack" }, command_id: "c-7" }),
    result: { schema_version: 1, replayed: false, code: "pfc1-0123", device: "D-1" },
  },
  {
    method: "answer",
    verb: "answer",
    viaVerb: true,
    args: { request: "R-1", rev: 1, text: "main", code: "pfc1-0123", command_id: "c-8" },
    run: (t) => t.answer({ request: "R-1", rev: 1, text: "main", code: "pfc1-0123", command_id: "c-8" }),
    result: { schema_version: 1, replayed: false, verb: "answer", device: "D-1" },
  },
  {
    method: "clear",
    verb: "clear",
    viaVerb: true,
    args: { request: "R-1", rev: 1, code: "pfc1-0123", command_id: "c-9" },
    run: (t) => t.clear({ request: "R-1", rev: 1, code: "pfc1-0123", command_id: "c-9" }),
    result: { schema_version: 1, replayed: false, verb: "clear", device: "D-1" },
  },
  {
    method: "ack",
    verb: "ack",
    viaVerb: true,
    args: { request: "R-1", rev: 1, code: "pfc1-0123", command_id: "c-10" },
    run: (t) => t.ack({ request: "R-1", rev: 1, code: "pfc1-0123", command_id: "c-10" }),
    result: { schema_version: 1, replayed: false, verb: "ack", device: "D-1", merged: true },
  },
  {
    method: "settingsGet",
    verb: "settings_get",
    viaVerb: true,
    args: { project_id: "P-1" },
    run: (t) => t.settingsGet({ project_id: "P-1" }),
    result: { schema_version: 1, project_id: "P-1", agent_id: null, settings: [] },
  },
  {
    method: "settingsGet (no arguments)",
    verb: "settings_get",
    viaVerb: true,
    args: {},
    run: (t) => t.settingsGet(),
    result: { schema_version: 1, project_id: null, agent_id: null, settings: [] },
  },
  {
    method: "settingsSet",
    verb: "settings_set",
    viaVerb: true,
    args: { key: "chat.self_turn_cap", value: 5, project_id: "P-1", command_id: "c-11" },
    run: (t) => t.settingsSet({ key: "chat.self_turn_cap", value: 5, project_id: "P-1", command_id: "c-11" }),
    result: { schema_version: 1, replayed: false, key: "chat.self_turn_cap" },
  },
  {
    method: "settingsUnset",
    verb: "settings_unset",
    viaVerb: true,
    args: { key: "chat.self_turn_cap", project_id: "P-1", command_id: "c-12" },
    run: (t) => t.settingsUnset({ key: "chat.self_turn_cap", project_id: "P-1", command_id: "c-12" }),
    result: { schema_version: 1, replayed: false, key: "chat.self_turn_cap" },
  },
  {
    method: "runList",
    verb: "run_list",
    viaVerb: true,
    args: {},
    run: (t) => t.runList(),
    result: { schema_version: 1, runs: [] },
  },
  {
    method: "confirmCode",
    verb: "confirm_code",
    viaVerb: true,
    args: { subject: { kind: "arm", proposal: "sp-1", plan_sha256: "a".repeat(64) }, command_id: "c-7" },
    run: (t) => t.confirmCode({ subject: { kind: "arm", proposal: "sp-1", plan_sha256: "a".repeat(64) }, command_id: "c-7" }),
    result: { schema_version: 1, replayed: false, code: "pfc1-x", expires_at: "2026-11-09T10:10:00Z", device: "D-1" },
  },
  {
    method: "arm",
    verb: "arm",
    viaVerb: true,
    args: { proposal: "sp-1", plan_sha256: "a".repeat(64), code: "pfc1-x", command_id: "c-8" },
    run: (t) => t.arm({ proposal: "sp-1", plan_sha256: "a".repeat(64), code: "pfc1-x", command_id: "c-8" }),
    result: { schema_version: 1, replayed: false, device: "D-1", proposal: "sp-1", run_id: "run-1", project_id: "P-1" },
  },
];

/** The scripted native events: one of each of the four shapes (the chat frame twice). */
const NATIVE: unknown[] = [
  { event: "frame", frame: { type: "caught", session_id: "S-1", last_seq: 6 } },
  { event: "frame", frame: { type: "delta", session_id: "S-1", text: "hi" } },
  { event: "request_frame", frame: { type: "requests", requests: [], claim: null } },
  { event: "reconnecting", after: 6, retry_in_ms: 4000, reason: "the server is stopping" },
  { event: "closed", error: { kind: "unauthorized", message: "this device was revoked; pair it again" } },
];
/** Every event before the last one: `closed` ends a stream. */
const BEFORE_CLOSED = NATIVE.slice(0, 4);
const CLOSED = NATIVE[4];

beforeEach(() => {
  FakeEventSource.all = [];
  FakeChannel.all = [];
});

afterEach(() => {
  vi.useRealTimers();
});

describe.each(RIGS)("the %s transport", (_name, makeRig) => {
  describe("the chat verbs", () => {
    it.each(CASES)("$method sends its verb with its arguments and returns the native answer", async (c) => {
      const rig = makeRig();
      rig.answerNext(c.result);
      await expect(c.run(rig.transport)).resolves.toEqual(c.result);
      expect(rig.lastCall()).toEqual({ target: target(rig, c), payload: payload(rig, c) });
    });

    it("a native failure with a refusal word reaches TransportError.refusal, and one without has none", async () => {
      for (const c of CASES.filter((x) => x.viaVerb)) {
        const rig = makeRig();
        rig.failNext("refused", "that code was used", "code_used");
        const err = (await c.run(rig.transport).catch((e: unknown) => e)) as TransportError;
        expect(err, c.method).toBeInstanceOf(TransportError);
        expect([err.kind, err.message, err.refusal], c.method).toEqual(["refused", "that code was used", "code_used"]);

        const plain = makeRig();
        plain.failNext("refused", "checks are red");
        const none = (await c.run(plain.transport).catch((e: unknown) => e)) as TransportError;
        expect(none.refusal, c.method).toBeUndefined();
      }
    });

    it.each(TRANSPORT_ERROR_KINDS)("a native failure of kind %s is a TransportError of that kind", async (kind: TransportErrorKind) => {
      expect(TRANSPORT_ERROR_KINDS).toHaveLength(9);
      for (const c of CASES) {
        const rig = makeRig();
        rig.failNext(kind, `text of ${kind}`);
        const err = await c.run(rig.transport).catch((e: unknown) => e);
        expect(err, c.method).toBeInstanceOf(TransportError);
        expect((err as TransportError).kind, c.method).toBe(kind);
        expect((err as TransportError).message, c.method).toBe(`text of ${kind}`);
      }
    });
  });

  describe("openStream", () => {
    async function open(rig: Rig, options = { session: "S-1", after: 0 }) {
      const seen: StreamEvent[] = [];
      if (rig.name === "tauri") rig.answerNext(7);
      const handle = await rig.transport.openStream(options, (e) => seen.push(e));
      return { seen, handle };
    }

    it("delivers the same scripted native events in the same order", async () => {
      const rig = makeRig();
      const { seen } = await open(rig);
      for (const native of BEFORE_CLOSED) rig.nativeEvent(native);
      expect(seen).toEqual(BEFORE_CLOSED);
    });

    it("sends nothing after closed, and closes the native stream on it", async () => {
      const rig = makeRig();
      const { seen } = await open(rig);
      for (const native of NATIVE) rig.nativeEvent(native);
      rig.nativeEvent({ event: "frame", frame: { type: "caught", session_id: "S-1", last_seq: 9 } });
      rig.nativeEvent({ event: "closed", error: { kind: "server", message: "again" } });
      expect(seen).toEqual(NATIVE);
      if (rig.name === "bridge") expect(FakeEventSource.all.at(-1)?.closed).toBe(true);
    });

    it("drops a value that is not one of the four event shapes", async () => {
      const rig = makeRig();
      const { seen } = await open(rig);
      const odd: unknown[] = [
        { event: "weird" },
        { event: "frame" },
        { event: "reconnecting", after: "6", retry_in_ms: 1, reason: "x" },
        { event: "closed", error: { kind: "not_a_kind", message: "x" } },
        { event: "closed", error: "oops" },
        // A request frame of an unknown type, or whose fields have no shape the state reads, is dropped.
        { event: "request_frame" },
        { event: "request_frame", frame: { type: "future_thing" } },
        { event: "request_frame", frame: { type: "request", request: "oops" } },
        { event: "request_frame", frame: { type: "request", request: { id: "R-1", revision: "1", status: "open" } } },
        { event: "request_frame", frame: { type: "requests", requests: 3, claim: null } },
        { event: "request_frame", frame: { type: "claim", claim: "bad" } },
        { event: "request_frame", frame: { type: "claim" } },
        42,
        null,
        "text",
      ];
      for (const value of odd) rig.nativeGarbage(rig.name === "bridge" ? JSON.stringify(value) : value);
      rig.nativeGarbage("{not json");
      expect(seen).toEqual([]);
      rig.nativeEvent(NATIVE[0]);
      expect(seen).toEqual([NATIVE[0]]);
    });

    it("close() closes the native stream and nothing follows", async () => {
      const rig = makeRig();
      const { seen, handle } = await open(rig);
      await handle.close();
      expect(rig.nativeClosed()).toBe(true);
      if (rig.name === "tauri") expect(rig.lastCall()).toEqual({ target: "close_stream", payload: { id: 7 } });
      rig.nativeEvent(NATIVE[0]);
      rig.nativeEvent(CLOSED);
      expect(seen).toEqual([]);
    });

    it("sends the options natively, and a missing `after` is 0 or absent and never ends in closed", async () => {
      const rig = makeRig();
      const seen: StreamEvent[] = [];
      if (rig.name === "tauri") rig.answerNext(1);
      await rig.transport.openStream({ session: "S-1" }, (e) => seen.push(e));
      if (rig.name === "bridge") {
        const url = new URL(FakeEventSource.all.at(-1)?.url ?? "", "http://127.0.0.1:1");
        expect(url.pathname).toBe("/bridge/stream");
        expect(url.searchParams.get("session")).toBe("S-1");
        expect([null, "0"]).toContain(url.searchParams.get("after"));
      } else {
        const call = rig.lastCall();
        expect(call.target).toBe("open_stream");
        const sent = (call.payload as { options: { session: string; after?: number } }).options;
        expect(sent.session).toBe("S-1");
        expect([undefined, 0]).toContain(sent.after);
        expect((call.payload as { onEvent: unknown }).onEvent).toBeInstanceOf(FakeChannel);
      }
      expect(seen).toEqual([]);
    });

    it("an after value goes through", async () => {
      const rig = makeRig();
      if (rig.name === "tauri") rig.answerNext(1);
      await rig.transport.openStream({ session: "S-2", after: 12 }, () => {});
      if (rig.name === "bridge") {
        expect(FakeEventSource.all.at(-1)?.url).toContain("after=12");
        expect(FakeEventSource.all.at(-1)?.url).toContain("session=S-2");
      } else {
        expect(rig.lastCall().payload).toMatchObject({ options: { session: "S-2", after: 12 } });
      }
    });

    it("never rejects for a stream failure: a failed open is one closed event", async () => {
      const rig = makeRig();
      const seen: StreamEvent[] = [];
      if (rig.name === "tauri") {
        rig.failOpen("unreachable", "cannot reach 1.2.3.4:7443");
        await expect(rig.transport.openStream({ session: "S-1" }, (e) => seen.push(e))).resolves.toBeDefined();
        expect(seen).toEqual([{ event: "closed", error: { kind: "unreachable", message: "cannot reach 1.2.3.4:7443" } }]);
      } else {
        await rig.transport.openStream({ session: "S-1" }, (e) => seen.push(e));
        FakeEventSource.all.at(-1)?.onerror?.({});
        // An error before any `closed` is `closed` `unreachable`, and the page never reconnects by itself.
        expect(seen).toEqual([{ event: "closed", error: { kind: "unreachable", message: "the bridge closed the stream" } }]);
        expect(FakeEventSource.all.at(-1)?.closed).toBe(true);
        FakeEventSource.all.at(-1)?.deliver(NATIVE[0]);
        FakeEventSource.all.at(-1)?.onerror?.({});
        expect(seen).toHaveLength(1);
      }
    });

    it("sets no timer and calls nothing on its own", async () => {
      vi.useFakeTimers();
      const rig = makeRig();
      const seen: StreamEvent[] = [];
      expect(vi.getTimerCount()).toBe(0);
      if (rig.name === "tauri") rig.answerNext(3);
      const handle = await rig.transport.openStream({ session: "S-1" }, (e) => seen.push(e));
      expect(vi.getTimerCount()).toBe(0);
      for (const native of NATIVE) rig.nativeEvent(native);
      expect(vi.getTimerCount()).toBe(0);
      await vi.advanceTimersByTimeAsync(10 * 60 * 1000);
      expect(vi.getTimerCount()).toBe(0);
      await handle.close();
      expect(vi.getTimerCount()).toBe(0);
    });
  });

  it("no call or argument holds the word token", async () => {
    const rig = makeRig();
    for (const c of CASES) {
      rig.answerNext(c.result);
      await c.run(rig.transport);
    }
    if (rig.name === "tauri") rig.answerNext(1);
    await rig.transport.openStream({ session: "S-1", after: 3 }, () => {});
    expect(rig.allCalls().toLowerCase()).not.toContain("token");
  });
});
