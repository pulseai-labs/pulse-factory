// AC-3: the composer's controls (Q5, P2, P3), the context gauge, the cache-warm timer and the cost in wu (P5).
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../App";
import { costLabel, dollarsTitle, formatWu, weightedUnits } from "../components/cost";
import { clock } from "../components/format";
import { alpha, bootChat, main, push, seeded } from "./chat-helpers";
import { FakeTransport, chatSession, claudeRuntime, frames } from "./fake-transport";

const select = (label: string) => screen.getByLabelText(label) as HTMLSelectElement;

function beta(patch = {}) {
  return chatSession("S-2", { project_id: "P-2", project_name: "beta", worktree_dir: "/home/dev/beta", ...patch });
}

describe("the controls come from the runtime's declaration (P2)", () => {
  it("renders one control per declared option, labelled by the option's label", async () => {
    await bootChat(seeded());
    await main().findByLabelText("Model");
    expect(select("Model").tagName).toBe("SELECT");
    expect(select("Effort")).toBeTruthy();
    expect(select("Permission mode")).toBeTruthy();
    // `artifacts` has the values `on` and `off`: a switch, not a pop-up.
    const artifacts = screen.getByRole("switch", { name: "Artifacts" }) as HTMLInputElement;
    expect(artifacts.type).toBe("checkbox");
    expect(Array.from(select("Effort").options).map((o) => o.value)).toEqual(["low", "medium", "high", "max"]);
  });

  it("shows the display name of a value and keeps the raw value (G3)", async () => {
    const fake = seeded({ sessions: [alpha({ model: "claude-opus-5-5", effort: "high", permission_mode: "manual" })] });
    fake.runtimes = [
      claudeRuntime(),
    ];
    const runtime = fake.runtimes[0];
    if (runtime) {
      const model = runtime.options.find((o) => o.id === "model");
      if (model) model.values = ["claude-opus-5-5", "claude-sonnet-5-5"];
      const effort = runtime.options.find((o) => o.id === "effort");
      if (effort) effort.values = ["low", "medium", "high", "xhigh", "max"];
      const mode = runtime.options.find((o) => o.id === "permission_mode");
      if (mode) mode.values = ["manual", "acceptEdits", "plan"];
    }
    await bootChat(fake);
    await main().findByLabelText("Model");
    const texts = (label: string) => Array.from(select(label).options).map((o) => [o.value, o.textContent]);
    expect(texts("Model")).toEqual([["claude-opus-5-5", "Opus 5.5"], ["claude-sonnet-5-5", "Sonnet 5.5"]]);
    expect(texts("Effort")).toContainEqual(["xhigh", "XHigh"]);
    expect(texts("Permission mode")).toEqual([["manual", "Ask before edits"], ["acceptEdits", "Edit automatically"], ["plan", "Plan mode"]]);
    // The select's value is the raw one, and its accessible label stays the option's label.
    expect(select("Model").value).toBe("claude-opus-5-5");
    expect(select("Permission mode").value).toBe("manual");
  });

  it("shows an extra option that the runtime declares", async () => {
    const fake = seeded();
    fake.runtimes = [
      claudeRuntime([{ id: "verbosity", label: "Verbosity", values: ["terse", "full"], default: "terse", free: false, live: true }]),
    ];
    await bootChat(fake);
    expect(await main().findByLabelText("Verbosity")).toBeTruthy();
    // Its value is the option's default when the session holds none.
    expect(select("Verbosity").value).toBe("terse");
  });

  it("shows no control for a session of a runtime that is not in the list", async () => {
    const fake = seeded({ sessions: [alpha({ runtime: "other-runtime" })] });
    await bootChat(fake);
    await waitFor(() => expect(fake.count("sessionList")).toBeGreaterThan(0));
    expect(screen.queryByLabelText("Effort")).toBeNull();
    expect(screen.queryByLabelText("Model")).toBeNull();
    expect(main().queryAllByRole("combobox")).toHaveLength(0);
  });

  it("shows the session's values, not the agent's, when they differ", async () => {
    const fake = seeded({
      sessions: [
        alpha({ effort: "low", model: "other-model", permission_mode: "manual", runtime_options: { artifacts: "on" } }),
      ],
    });
    // The agent's defaults are high, fake-model, bypass and off.
    expect(fake.agents[0]?.effort).toBe("high");
    await bootChat(fake);
    await main().findByLabelText("Effort");
    expect(select("Effort").value).toBe("low");
    expect(select("Model").value).toBe("other-model");
    expect(select("Permission mode").value).toBe("manual");
    expect((screen.getByRole("switch", { name: "Artifacts" }) as HTMLInputElement).checked).toBe(true);
  });

  it("shows a free option's current value even when it is not among the values", async () => {
    const fake = seeded({ sessions: [alpha({ model: "claude-opus-5-5" })] });
    await bootChat(fake);
    await main().findByLabelText("Model");
    expect(select("Model").value).toBe("claude-opus-5-5");
    expect(Array.from(select("Model").options).map((o) => o.value)).toContain("claude-opus-5-5");
  });
});

describe("the controls of 11a: an icon, the display name, and as wide as the label (r1.s6.w2)", () => {
  const control = (label: string) => (screen.getByLabelText(label) as HTMLElement).closest(".control") as HTMLElement;

  it("each control renders its icon before its display name, and a chevron after it", async () => {
    const fake = seeded({ sessions: [alpha({ model: "claude-opus-5-5", effort: "xhigh", permission_mode: "manual" })] });
    const runtime = fake.runtimes[0];
    const model = runtime?.options.find((o) => o.id === "model");
    if (model) model.values = ["claude-opus-5-5", "claude-sonnet-5-5"];
    const effort = runtime?.options.find((o) => o.id === "effort");
    if (effort) effort.values = ["low", "xhigh"];
    const mode = runtime?.options.find((o) => o.id === "permission_mode");
    if (mode) mode.values = ["manual", "plan"];
    await bootChat(fake);
    await main().findByLabelText("Model");

    for (const [label, icon, text] of [
      ["Model", "sparkles", "Opus 5.5"],
      ["Effort", "zap", "XHigh"],
      ["Permission mode", "lock", "Ask before edits"],
    ] as const) {
      const el = control(label);
      expect(el.querySelector(`[data-icon=${icon}]`), `${label} icon`).not.toBeNull();
      expect(el.querySelector("[data-icon=chevron-down]"), `${label} chevron`).not.toBeNull();
      // The shown text is the display name of the chosen value, and the pop-up keeps the raw one.
      expect(el.querySelector(".control-text")?.textContent, label).toBe(text);
    }
  });

  it("the visible text is the label's width: the pop-up lies over it, so a longer option does not widen the control", async () => {
    const fake = seeded({ sessions: [alpha({ effort: "low" })] });
    await bootChat(fake);
    await main().findByLabelText("Effort");
    const el = control("Effort");
    const field = el.querySelector("select") as HTMLSelectElement;
    // The pop-up is a transparent overlay of the face (the icon, the text and the chevron); its own width
    // never sets the control's.
    expect(field.parentElement?.className).toContain("control-face");
    expect(field.className).toContain("control-field");
    expect(el.querySelector(".control-text")?.textContent).toBe("Low");
  });

  it("the Artifacts switch keeps its label and gets the layers icon", async () => {
    await bootChat(seeded());
    await main().findByLabelText("Artifacts");
    const el = (screen.getByRole("switch", { name: "Artifacts" }) as HTMLElement).closest(".control") as HTMLElement;
    expect(el.textContent).toContain("Artifacts");
    expect(el.querySelector("[data-icon=layers]")).not.toBeNull();
  });

  it("Attach is the paperclip, not built yet: disabled, tagged `Later release`, with no click effect", async () => {
    const fake = seeded();
    const user = await bootChat(fake);
    const attach = main().getByText("Attach").closest("[data-not-built]") as HTMLElement;
    expect(attach.getAttribute("aria-disabled")).toBe("true");
    expect(within(attach).getByText("Later release")).toBeTruthy();
    expect(attach.querySelector("[data-icon=paperclip]")).not.toBeNull();
    // It sits left of the controls.
    const group = screen.getByRole("group", { name: "Chat controls" });
    expect(attach.compareDocumentPosition(group) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    const calls = [...fake.calls];
    await user.click(attach);
    expect(fake.calls).toEqual(calls);
    expect(document.activeElement).not.toBe(attach);
  });
});

describe("a change applies to that chat only (P2, P3)", () => {
  it("sends one sessionSet with that field and that session's ID, and shows the answer", async () => {
    const fake = seeded();
    const user = await bootChat(fake);
    await main().findByLabelText("Effort");
    await user.selectOptions(select("Effort"), "medium");

    await waitFor(() => expect(fake.setArgs).toHaveLength(1));
    expect(fake.setArgs[0]).toEqual({ session_id: "S-1", effort: "medium", command_id: expect.stringMatching(/^[0-9a-f-]{16,}$/) });
    await waitFor(() => expect(select("Effort").value).toBe("medium"));
  });

  it("sends a model and a permission mode as their own fields", async () => {
    const fake = seeded();
    const user = await bootChat(fake);
    await main().findByLabelText("Model");
    await user.selectOptions(select("Permission mode"), "manual");
    await waitFor(() => expect(fake.setArgs).toHaveLength(1));
    expect(fake.setArgs[0]).toMatchObject({ session_id: "S-1", permission_mode: "manual" });
    expect(Object.keys(fake.setArgs[0] ?? {}).sort()).toEqual(["command_id", "permission_mode", "session_id"]);

    await user.selectOptions(select("Model"), "other-model");
    await waitFor(() => expect(fake.setArgs).toHaveLength(2));
    expect(Object.keys(fake.setArgs[1] ?? {}).sort()).toEqual(["command_id", "model", "session_id"]);
  });

  it("sends a runtime option inside `options`, and says it applies when the chat restarts", async () => {
    const fake = seeded();
    const user = await bootChat(fake);
    await main().findByLabelText("Artifacts");
    const artifacts = () => screen.getByRole("switch", { name: "Artifacts" }) as HTMLInputElement;
    expect(artifacts().checked).toBe(false);
    expect(screen.queryByText("applies when the chat restarts")).toBeNull();
    await user.click(artifacts());

    await waitFor(() => expect(fake.setArgs).toHaveLength(1));
    expect(fake.setArgs[0]).toEqual({ session_id: "S-1", options: { artifacts: "on" }, command_id: expect.any(String) });
    // `artifacts` is not live: it waits in `pending_options`.
    expect(await main().findByText("applies when the chat restarts")).toBeTruthy();
    expect(artifacts().checked).toBe(true);
  });

  it("shows the note of an answer inline, and the server's text for a refusal", async () => {
    const fake = seeded();
    const user = await bootChat(fake);
    await main().findByLabelText("Effort");
    fake.noteOnce("sessionSet", "the change stopped the process; the next send reopens it");
    await user.selectOptions(select("Effort"), "max");
    expect(await main().findByText("the change stopped the process; the next send reopens it")).toBeTruthy();

    fake.fail("sessionSet", "refused", 'effort "ultra" is not declared; the values are low, medium, high, max', 1);
    await user.selectOptions(select("Effort"), "low");
    expect(await main().findByText(/is not declared/)).toBeTruthy();
    // The control shows the session's value again.
    expect(select("Effort").value).toBe("max");
  });

  it("keeps a second chat's own value, and the Agents page keeps the default", async () => {
    const fake = seeded({ sessions: [alpha(), beta()] });
    const user = await bootChat(fake);
    await main().findByLabelText("Effort");
    await user.selectOptions(select("Effort"), "medium");
    await waitFor(() => expect(select("Effort").value).toBe("medium"));

    // The other chat opens from its sidebar row and shows its own effort.
    const sidebar = within(screen.getByRole("region", { name: "Sidebar" }));
    await user.click(sidebar.getByRole("button", { name: /session in beta:/ }));
    await waitFor(() => expect(fake.current()?.options.session).toBe("S-2"));
    await push(fake, frames.hello(fake.sessions, "S-2"));
    await waitFor(() => expect(main().getByLabelText("Project").textContent).toBe("beta"));
    expect(select("Effort").value).toBe("high");

    // Back to the first: it kept its value. Only one sessionSet went out.
    await user.click(screen.getByRole("tab", { name: "Orchestrator · alpha" }));
    await waitFor(() => expect(main().getByLabelText("Project").textContent).toBe("alpha"));
    expect(select("Effort").value).toBe("medium");
    expect(fake.setArgs).toHaveLength(1);

    // The agent's default is untouched.
    await user.click(within(screen.getByRole("navigation")).getByRole("button", { name: "Agents" }));
    const effort = await within(screen.getByRole("main")).findByRole("group", { name: "Reasoning effort" });
    expect(within(effort).getByText("High")).toBeTruthy();
    expect(within(effort).queryByText("Medium")).toBeNull();
    expect(fake.agents[0]?.effort).toBe("high");
  });

  it("has no control that changes an agent", async () => {
    const fake = seeded();
    const user = await bootChat(fake);
    await main().findByLabelText("Effort");
    await user.selectOptions(select("Effort"), "low");
    await waitFor(() => expect(fake.setArgs).toHaveLength(1));
    // The only calls that wrote anything are the chat verbs.
    const writes = fake.calls.filter((c) => c.startsWith("session") && c !== "sessionList");
    expect(writes).toEqual(["sessionSet"]);
  });
});

describe("the context ring", () => {
  const arc = () => screen.getByLabelText("Context gauge").querySelector(".ring-arc") as SVGCircleElement | null;

  it("shows round(100 · used / max) percent with a ring at that share", async () => {
    const fake = seeded({ sessions: [alpha({ context: { used_tokens: 620, max_tokens: 1000 } })] });
    await bootChat(fake);
    expect(screen.getByLabelText("Context gauge").textContent).toBe("62%");
    const [drawn, whole] = (arc()?.getAttribute("stroke-dasharray") ?? "").split(" ").map(Number);
    expect((drawn ?? 0) / (whole ?? 1)).toBeCloseTo(0.62, 2);
    expect(screen.getByLabelText("Context gauge").getAttribute("title")).toBe("Context: 620 of 1000 tokens");
  });

  it("rounds to the nearest percent", async () => {
    const fake = seeded({ sessions: [alpha({ context: { used_tokens: 1, max_tokens: 3 } })] });
    await bootChat(fake);
    expect(screen.getByLabelText("Context gauge").textContent).toBe("33%");
  });

  it("shows a dash and no arc when the context is not known", async () => {
    await bootChat(seeded({ sessions: [alpha({ context: null })] }));
    expect(screen.getByLabelText("Context gauge").textContent).toBe("—");
    expect(arc()).toBeNull();
  });

  it("shows a dash for a window of zero", async () => {
    await bootChat(seeded({ sessions: [alpha({ context: { used_tokens: 5, max_tokens: 0 } })] }));
    expect(screen.getByLabelText("Context gauge").textContent).toBe("—");
  });
});

describe("Send, Stop and the compact box (11a)", () => {
  it("shows a text-less Send arrow while the agent is idle, disabled until there is text", async () => {
    const user = await bootChat(seeded());
    const send = screen.getByRole("button", { name: "Send" }) as HTMLButtonElement;
    expect(send.textContent).toBe("");
    expect(send.querySelector("svg")).not.toBeNull();
    expect(send.disabled).toBe(true);
    await user.type(screen.getByRole("textbox", { name: "Message Orchestrator" }), "hi");
    expect(send.disabled).toBe(false);
    expect(screen.queryByRole("button", { name: "Stop" })).toBeNull();
  });

  it("shows Stop in Send's place while the state is working, and Stop calls sessionInterrupt", async () => {
    const fake = seeded({ sessions: [alpha({ state: "working" })] });
    const user = await bootChat(fake);
    expect(screen.queryByRole("button", { name: "Send" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Stop" }));
    await waitFor(() => expect(fake.interruptArgs).toHaveLength(1));
    expect(fake.count("sessionSend")).toBe(0);
  });

  it("keeps the textbox's name `Message <agent>` with the label visually hidden, and no hint line under the box (NG-7)", async () => {
    await bootChat(seeded({ sessions: [alpha({ state: "working" })] }));
    expect(screen.getByRole("textbox", { name: "Message Orchestrator" })).toBeTruthy();
    expect(document.querySelector("label.ui-sr-only")?.textContent).toBe("Message Orchestrator");
    // The line leaves: no text of it is anywhere, in either form.
    expect(document.body.textContent).not.toMatch(/steers the running turn/i);
    expect(document.body.textContent).not.toMatch(/runs as the next turn/i);
  });

  it("while the agent works Send shows beside Stop once there is text, and its title says what a message does", async () => {
    const user = await bootChat(seeded({ sessions: [alpha({ state: "working" })] }));
    // No text: Stop alone, as 11a draws a working turn.
    expect(screen.queryByRole("button", { name: "Send" })).toBeNull();
    await user.type(screen.getByRole("textbox", { name: "Message Orchestrator" }), "wait, also this");
    expect(screen.getByRole("button", { name: "Send" }).getAttribute("title")).toBe("Steers the running turn");
    expect(screen.getByRole("button", { name: "Stop" })).toBeTruthy();
  });

  it("says `Runs as the next turn` in Send's title when the runtime does not steer", async () => {
    const fake = seeded({ sessions: [alpha({ state: "working" })] });
    fake.runtimes = [claudeRuntime([], { mid_turn: "next_turn" })];
    const user = await bootChat(fake);
    await user.type(screen.getByRole("textbox", { name: "Message Orchestrator" }), "later");
    expect(screen.getByRole("button", { name: "Send" }).getAttribute("title")).toBe("Runs as the next turn");
  });

  it("shows the ended panel with Start session instead of the box", async () => {
    const fake = seeded();
    await bootChat(fake);
    await push(fake, frames.session(alpha({ ended_at: "2026-11-09T11:00:00Z", updated_at: "2026-11-09T11:00:00Z" })));
    expect(main().getByText("This session has ended.")).toBeTruthy();
    expect(screen.queryByRole("group", { name: "Chat controls" })).toBeNull();
  });
});

describe("the cache-warm timer, under fake timers", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-11-09T10:09:00Z"));
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  async function advance(ms: number) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(ms);
    });
  }

  async function bootFake(fake: FakeTransport) {
    render(<App transport={fake} />);
    await advance(0);
    await push(fake, frames.hello(fake.sessions));
    fireEvent.click(within(screen.getByRole("region", { name: "Sidebar" })).getByRole("button", { name: /session in alpha:/ }));
    await advance(0);
    await push(fake, frames.hello(fake.sessions, "S-1"));
    await advance(0);
  }

  it("goes from warm to cold with no transport call", async () => {
    const fake = seeded({ sessions: [alpha({ last_turn_at: "2026-11-09T10:00:00Z", cache_ttl_seconds: 3600 })] });
    await bootFake(fake);
    expect(screen.getByText("Cache warm · 51m left")).toBeTruthy();
    const calls = [...fake.calls];

    await advance(30 * 60 * 1000);
    expect(screen.getByText("Cache warm · 21m left")).toBeTruthy();
    await advance(20 * 60 * 1000);
    expect(screen.getByText("Cache warm · 1m left")).toBeTruthy();
    await advance(60 * 1000);
    expect(screen.getByText("Cache cold")).toBeTruthy();
    expect(screen.queryByText(/Cache warm/)).toBeNull();
    expect(fake.calls).toEqual(calls);

    // Cold: no timer is left to call anything, and an hour makes no call.
    await advance(60 * 60 * 1000);
    expect(fake.calls).toEqual(calls);
    expect(vi.getTimerCount()).toBe(0);
  });

  it("uses the 5-minute TTL of the last turn", async () => {
    const fake = seeded({ sessions: [alpha({ last_turn_at: "2026-11-09T10:06:00Z", cache_ttl_seconds: 300 })] });
    await bootFake(fake);
    expect(screen.getByText("Cache warm · 2m left")).toBeTruthy();
    await advance(2 * 60 * 1000);
    expect(screen.getByText("Cache cold")).toBeTruthy();
  });

  it("shows `Cache cold` before the first turn, when nothing is cached, with no title", async () => {
    const fake = seeded({ sessions: [alpha({ last_turn_at: null, cache_ttl_seconds: null })] });
    await bootFake(fake);
    expect(screen.getByText("Cache cold")).toBeTruthy();
    expect(screen.queryByText(/Cache warm/)).toBeNull();
    expect(screen.getByText("Cache cold").closest(".cache-text")?.hasAttribute("title")).toBe(false);
    // Nothing is warm: no timer runs.
    expect(vi.getTimerCount()).toBe(0);
  });

  it("puts 11b's sentence on the line as its title where the record gives the numbers", async () => {
    const fake = seeded({
      sessions: [alpha({ last_turn_at: "2026-11-09T10:00:00Z", cache_ttl_seconds: 3600, context: { used_tokens: 124000, max_tokens: 200000 } })],
    });
    await bootFake(fake);
    const line = screen.getByText("Cache warm · 51m left").closest(".cache-text") as HTMLElement;
    const until = clock(Date.parse("2026-11-09T11:00:00Z"));
    expect(line.getAttribute("title")).toBe(
      `Prompt cache stays warm for 1 hour after the last reply. A reply before ${until} reuses 124k cached tokens.`,
    );
  });

  it("names the 5-minute TTL in the title, and leaves out the second sentence when the context is not known", async () => {
    const fake = seeded({ sessions: [alpha({ last_turn_at: "2026-11-09T10:06:00Z", cache_ttl_seconds: 300, context: null })] });
    await bootFake(fake);
    const line = screen.getByText("Cache warm · 2m left").closest(".cache-text") as HTMLElement;
    expect(line.getAttribute("title")).toBe("Prompt cache stays warm for 5 minutes after the last reply.");
  });

  it("makes no call for an hour on a chat with a warm cache, blurred or focused", async () => {
    const fake = seeded({ sessions: [alpha({ last_turn_at: "2026-11-09T10:09:00Z", cache_ttl_seconds: 3600 })] });
    await bootFake(fake);
    const calls = [...fake.calls];
    for (let i = 0; i < 2; i++) {
      await advance(30 * 60 * 1000);
      await act(async () => {
        window.dispatchEvent(new Event("blur"));
        document.dispatchEvent(new Event("visibilitychange"));
        window.dispatchEvent(new Event("focus"));
        window.dispatchEvent(new Event("online"));
      });
    }
    expect(fake.calls).toEqual(calls);
  });
});

describe("the cost in wu", () => {
  const usage = (over = {}) => ({
    input_tokens: 0,
    output_tokens: 0,
    cache_read_tokens: 0,
    cache_write_tokens: 0,
    cost_micros: 0,
    ...over,
  });

  it("weights input 1, output 5, cache read 0.1 and a cache write 2 (1 hour) or 1.25 (5 minutes)", () => {
    const u = usage({ input_tokens: 1000, output_tokens: 200, cache_read_tokens: 5000, cache_write_tokens: 100 });
    expect(weightedUnits(u, 3600)).toBeCloseTo(1000 + 1000 + 500 + 200);
    expect(weightedUnits(u, 300)).toBeCloseTo(1000 + 1000 + 500 + 125);
    // No TTL is the 5-minute weight.
    expect(weightedUnits(u, null)).toBeCloseTo(2625);
    expect(weightedUnits(usage(), 3600)).toBe(0);
  });

  it("formats under 1000 as whole units, then as k and M with one decimal", () => {
    expect(formatWu(0)).toBe("0 wu");
    expect(formatWu(0.4)).toBe("0 wu");
    expect(formatWu(0.5)).toBe("1 wu");
    expect(formatWu(999)).toBe("999 wu");
    expect(formatWu(999.4)).toBe("999 wu");
    expect(formatWu(1000)).toBe("1.0k wu");
    expect(formatWu(1500)).toBe("1.5k wu");
    expect(formatWu(999_949)).toBe("999.9k wu");
    expect(formatWu(999_950)).toBe("1.0M wu");
    expect(formatWu(1_900_000)).toBe("1.9M wu");
    expect(formatWu(12_345_678)).toBe("12.3M wu");
  });

  it("puts the weighted units and the dollars together for one record", () => {
    const u = usage({ input_tokens: 4000, output_tokens: 1000, cost_micros: 1_234_567 });
    expect(costLabel(u, 3600)).toBe("9.0k wu");
    expect(dollarsTitle(u)).toBe("$1.234567 at API prices");
    expect(dollarsTitle(usage())).toBe("$0 at API prices");
  });

  it("shows the weighted units in the header, with the dollars as the title", async () => {
    const fake = seeded({
      sessions: [
        alpha({
          usage: { input_tokens: 4000, output_tokens: 1000, cache_read_tokens: 0, cache_write_tokens: 0, cost_micros: 1_234_567 },
          cache_ttl_seconds: 3600,
        }),
      ],
    });
    await bootChat(fake);
    const cost = screen.getByLabelText("Cost");
    expect(cost.textContent).toBe("9.0k wu");
    expect(cost.getAttribute("title")).toBe("$1.234567 at API prices");
    // The sidebar does not show dollars next to wu: the two never add up into one number.
    expect(screen.queryByText(/^\$/)).toBeNull();
  });

  it("updates the header when a session frame brings a new cost", async () => {
    const fake = seeded();
    await bootChat(fake);
    expect(screen.getByLabelText("Cost").textContent).toBe("0 wu");
    await push(
      fake,
      frames.session(
        alpha({
          updated_at: "2026-11-09T10:10:00Z",
          usage: { input_tokens: 10, output_tokens: 2, cache_read_tokens: 0, cache_write_tokens: 0, cost_micros: 3465 },
        }),
      ),
    );
    expect(screen.getByLabelText("Cost").textContent).toBe("20 wu");
    expect(screen.getByLabelText("Cost").getAttribute("title")).toBe("$0.003465 at API prices");
  });
});
