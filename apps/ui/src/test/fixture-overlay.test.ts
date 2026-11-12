// @vitest-environment node
// The overlay of the d25 fixture server (`e2e/screens/server.ts`) is how a round-3 item changes another
// module's value for one test. These checks run the seam itself: the w4-shaped case is a function patch on
// `show` for a request id whose base fixture throws.
import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";
import { clearOverlay, overlay, startFixtureServer, type FixtureServer } from "../../e2e/screens/server";

let server: FixtureServer;

beforeAll(async () => {
  server = await startFixtureServer(process.cwd());
});
afterAll(async () => {
  await server.close();
});
afterEach(() => {
  clearOverlay();
  server.unexpected.length = 0;
});

async function call(verb: string, args: unknown): Promise<{ ok: boolean; result?: unknown }> {
  const res = await fetch(`${server.origin}/bridge/verbs/${verb}`, { method: "POST", body: JSON.stringify(args) });
  return (await res.json()) as { ok: boolean; result?: unknown };
}

describe("the d25 overlay", () => {
  it("answers a verb for an id the base fixture does not know, through a function patch", async () => {
    overlay({ show: (base: unknown, args: unknown) => ({ base, id: (args as { id: string }).id }) });
    const answer = await call("show", { id: "only-in-the-overlay" });
    expect(answer).toEqual({ ok: true, result: { id: "only-in-the-overlay" } });
    expect(server.unexpected).toEqual([]);
  });

  it("hands the base answer to a function patch when the base has one", async () => {
    overlay({ requests: (base: unknown) => ({ wrapped: base }) });
    const answer = (await call("requests", {})) as { ok: boolean; result: { wrapped: unknown } };
    expect(answer.ok).toBe(true);
    expect(answer.result.wrapped).toBeDefined();
  });

  it("answers a plain patch without asking the base", async () => {
    overlay({ show: { plain: true } });
    expect(await call("show", { id: "unknown" })).toEqual({ ok: true, result: { plain: true } });
    expect(server.unexpected).toEqual([]);
  });

  it("still fails a verb with no fixture and no overlay", async () => {
    const answer = await call("show", { id: "unknown" });
    expect(answer.ok).toBe(false);
    expect(server.unexpected).toHaveLength(1);
  });
});
