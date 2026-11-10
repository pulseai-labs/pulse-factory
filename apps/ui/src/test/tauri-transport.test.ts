// The Tauri transport (docs/contracts/ui-transport.md): each method invokes the command of its name,
// a rejection {kind, message} becomes a TransportError of that kind, and main.tsx picks this
// transport inside Tauri and the bridge transport elsewhere. `invoke` is mocked: no Tauri runs here.
import type { ProjectAddArgs } from "@pulse-factory/api-client";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { createTauriTransport } from "../transport/tauri";
import { TRANSPORT_ERROR_KINDS, TransportError } from "../transport/types";

const invoke = vi.fn();
const isTauri = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
  isTauri: () => isTauri(),
  Channel: class {
    onmessage: (message: unknown) => void = () => {};
  },
}));

const DEVICE = {
  id: "D-1",
  name: "macbook",
  paired_at: "2026-11-08T00:00:00Z",
  last_seen_at: null,
  last_peer: null,
  revoked_at: null,
};

beforeEach(() => {
  invoke.mockReset();
  isTauri.mockReset();
});

describe("the Tauri transport invokes the command of each method's name", () => {
  it("paired takes no arguments and returns the server or null", async () => {
    invoke.mockResolvedValueOnce({ address: "100.90.1.2:7443", device: DEVICE });
    const t = createTauriTransport();
    await expect(t.paired()).resolves.toEqual({ address: "100.90.1.2:7443", device: DEVICE });
    expect(invoke).toHaveBeenCalledWith("paired", undefined);
    invoke.mockResolvedValueOnce(null);
    await expect(t.paired()).resolves.toBeNull();
  });

  it("pair sends the code and the device name", async () => {
    invoke.mockResolvedValueOnce({ address: "a:1", device: DEVICE });
    await createTauriTransport().pair("pf1-abc", "macbook");
    expect(invoke).toHaveBeenCalledWith("pair", { code: "pf1-abc", deviceName: "macbook" });
  });

  it("status and projectList take no arguments", async () => {
    invoke.mockResolvedValue({ ok: 1 });
    const t = createTauriTransport();
    await t.status();
    await t.projectList();
    expect(invoke).toHaveBeenNthCalledWith(1, "status", undefined);
    expect(invoke).toHaveBeenNthCalledWith(2, "project_list", undefined);
  });

  it("projectAdd sends its arguments as args", async () => {
    invoke.mockResolvedValueOnce({ replayed: false });
    const args: ProjectAddArgs = { root: "/work/alpha", name: "alpha", command_id: "cmd-1" };
    await createTauriTransport().projectAdd(args);
    expect(invoke).toHaveBeenCalledWith("project_add", { args });
  });

  it("confirmCode and arm invoke the generic verb command with {name, args} (O2)", async () => {
    invoke.mockResolvedValue({ ok: 1 });
    const t = createTauriTransport();
    const confirmArgs = { subject: { kind: "arm" as const, proposal: "sp-1", plan_sha256: "a".repeat(64) }, command_id: "c-1" };
    const armArgs = { proposal: "sp-1", plan_sha256: "a".repeat(64), code: "pfc1-x", command_id: "c-2" };
    await t.confirmCode(confirmArgs);
    await t.arm(armArgs);
    expect(invoke).toHaveBeenNthCalledWith(1, "verb", { name: "confirm_code", args: confirmArgs });
    expect(invoke).toHaveBeenNthCalledWith(2, "verb", { name: "arm", args: armArgs });
  });

  it("forget invokes forget and returns nothing", async () => {
    invoke.mockResolvedValueOnce(null);
    await expect(createTauriTransport().forget()).resolves.toBeUndefined();
    expect(invoke).toHaveBeenCalledWith("forget", undefined);
  });

  it("returns what the command returns, and no method takes a token", async () => {
    invoke.mockResolvedValue({ schema_version: 1, projects: [] });
    const out = await createTauriTransport().projectList();
    expect(out).toEqual({ schema_version: 1, projects: [] });
    for (const call of invoke.mock.calls) {
      expect(JSON.stringify(call)).not.toMatch(/token/i);
    }
  });
});

describe("a rejection becomes a TransportError of its kind", () => {
  const methods: Array<[string, (t: ReturnType<typeof createTauriTransport>) => Promise<unknown>]> = [
    ["paired", (t) => t.paired()],
    ["pair", (t) => t.pair("pf1-x", "m")],
    ["status", (t) => t.status()],
    ["projectList", (t) => t.projectList()],
    ["projectAdd", (t) => t.projectAdd({ root: "/r", name: null, command_id: "c" } as ProjectAddArgs)],
    ["forget", (t) => t.forget()],
    ["sessionList", (t) => t.sessionList()],
    ["sessionStart", (t) => t.sessionStart({ project_id: "P-1", agent_id: "orchestrator", command_id: "c" })],
    ["sessionSend", (t) => t.sessionSend({ session_id: "S-1", text: "x", command_id: "c" })],
    ["sessionAnswer", (t) => t.sessionAnswer({ session_id: "S-1", seq: 1, answer: { type: "allow" }, command_id: "c" })],
    ["sessionInterrupt", (t) => t.sessionInterrupt({ session_id: "S-1", command_id: "c" })],
    ["sessionSet", (t) => t.sessionSet({ session_id: "S-1", effort: "low", command_id: "c" })],
    ["sessionEnd", (t) => t.sessionEnd({ session_id: "S-1", command_id: "c" })],
    ["confirmCode", (t) => t.confirmCode({ subject: { kind: "arm", proposal: "sp-1", plan_sha256: "a".repeat(64) }, command_id: "c" })],
    ["arm", (t) => t.arm({ proposal: "sp-1", plan_sha256: "a".repeat(64), code: "pfc1-x", command_id: "c" })],
  ];

  it.each(TRANSPORT_ERROR_KINDS)("kind %s", async (kind) => {
    expect(TRANSPORT_ERROR_KINDS).toHaveLength(9);
    for (const [name, call] of methods) {
      invoke.mockRejectedValueOnce({ kind, message: `text of ${kind}` });
      const err = await call(createTauriTransport()).catch((e: unknown) => e);
      expect(err, name).toBeInstanceOf(TransportError);
      expect((err as TransportError).kind, name).toBe(kind);
      expect((err as TransportError).message, name).toBe(`text of ${kind}`);
    }
  });

  it("an unknown kind or a rejection of another shape is a server error", async () => {
    const t = createTauriTransport();
    invoke.mockRejectedValueOnce({ kind: "weird", message: "x" });
    const a = await t.status().catch((e: unknown) => e);
    expect((a as TransportError).kind).toBe("server");
    invoke.mockRejectedValueOnce("a plain string");
    const b = await t.status().catch((e: unknown) => e);
    expect(b).toBeInstanceOf(TransportError);
    expect((b as TransportError).kind).toBe("server");
    invoke.mockRejectedValueOnce(new Error("boom"));
    const c = await t.status().catch((e: unknown) => e);
    expect((c as TransportError).kind).toBe("server");
  });

  it("a rejection that is not a message of the contract never shows its own text", async () => {
    invoke.mockRejectedValueOnce({ kind: "server" });
    const err = (await createTauriTransport()
      .status()
      .catch((e: unknown) => e)) as TransportError;
    expect(err.kind).toBe("server");
    expect(err.message).not.toBe("");
  });
});

describe("main.tsx picks the transport", () => {
  async function runMain(inTauri: boolean) {
    vi.resetModules();
    isTauri.mockReturnValue(inTauri);
    document.body.innerHTML = '<div id="root"></div>';
    let rendered: { props: { transport: { paired(): Promise<unknown> } } } | undefined;
    vi.doMock("react-dom/client", () => ({
      createRoot: () => ({
        render: (el: typeof rendered) => {
          rendered = el;
        },
      }),
    }));
    vi.doMock("../App", () => ({ App: () => null }));
    vi.doMock("@fontsource-variable/geist", () => ({}));
    vi.doMock("@fontsource-variable/geist-mono", () => ({}));
    const fetchSpy = vi.fn().mockResolvedValue(
      new Response(JSON.stringify({ ok: true, result: null }), { status: 200 }),
    );
    vi.stubGlobal("fetch", fetchSpy);
    invoke.mockResolvedValue(null);
    await import("../main");
    expect(rendered).toBeDefined();
    await rendered?.props.transport.paired();
    return { fetchSpy };
  }

  it("uses invoke inside Tauri and no fetch", async () => {
    const { fetchSpy } = await runMain(true);
    expect(invoke).toHaveBeenCalledWith("paired", undefined);
    expect(fetchSpy).not.toHaveBeenCalled();
  });

  it("uses the bridge over fetch elsewhere and no invoke", async () => {
    const { fetchSpy } = await runMain(false);
    expect(fetchSpy).toHaveBeenCalledWith("/bridge/paired", expect.anything());
    expect(invoke).not.toHaveBeenCalled();
  });
});
