// AC-1: the Agents rail item, the Orchestrator's page (6a) and Start session.
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../App";
import { FakeTransport, chatSession, claudeRuntime, orchestrator, project } from "./fake-transport";

function seeded(): FakeTransport {
  const fake = new FakeTransport({ projects: [project(1, "alpha"), project(2, "beta")] });
  fake.runtimes = [claudeRuntime()];
  fake.agents = [orchestrator()];
  return fake;
}

async function open(fake: FakeTransport) {
  const user = userEvent.setup();
  render(<App transport={fake} />);
  await screen.findByRole("navigation");
  await waitFor(() => expect(fake.count("projectList")).toBe(1));
  return user;
}

async function openAgentsPage(fake: FakeTransport) {
  const user = await open(fake);
  await user.click(within(screen.getByRole("navigation")).getByRole("button", { name: "Agents" }));
  await within(screen.getByRole("main")).findByRole("heading", { name: "Agents" });
  await waitFor(() => expect(fake.count("sessionList")).toBe(1));
  return user;
}

async function openAgents(fake: FakeTransport) {
  const user = await openAgentsPage(fake);
  await screen.findByRole("list", { name: "Sessions by project" });
  return user;
}

const rows = () => within(screen.getByRole("list", { name: "Sessions by project" })).getAllByRole("listitem");
const rowOf = (name: string) => {
  const row = rows().find((li) => within(li).queryByText(name, { exact: true }) !== null);
  if (!row) throw new Error(`no row for ${name}`);
  return within(row);
};

describe("the Agents page (6a)", () => {
  it("puts Agents after Needs you on the rail, with Settings last and nothing else a button", async () => {
    await open(seeded());
    const names = within(screen.getByRole("navigation"))
      .getAllByRole("button")
      .map((b) => b.getAttribute("aria-label"));
    expect(names).toEqual(["Projects", "Needs you", "Agents", "Settings"]);
  });

  it("shows the Orchestrator's defaults from the server's agent list, read only", async () => {
    const fake = seeded();
    fake.agents = [orchestrator({ effort: "high", model: "fake-model", skills: ["pulse-factory", "review"] })];
    await openAgents(fake);

    const main = within(screen.getByRole("main"));
    expect(main.getByRole("heading", { name: "Orchestrator" })).toBeTruthy();
    expect(main.getByText("Talks to a project's team")).toBeTruthy();
    const facts = main.getByRole("list", { name: "Defaults" });
    const defaults = within(facts);
    expect(defaults.getByText("claude-code")).toBeTruthy();
    expect(defaults.getByText("fake-model")).toBeTruthy();
    expect(defaults.getByText("high")).toBeTruthy();
    expect(defaults.getByText("bypass")).toBeTruthy();
    expect(defaults.getByText("pulse-factory, review")).toBeTruthy();
    expect(defaults.getByText("artifacts: off")).toBeTruthy();
    // P1: no edit control in Release 1.
    expect(main.queryByRole("textbox")).toBeNull();
    expect(main.queryByRole("combobox")).toBeNull();
    expect(main.queryByRole("button", { name: /edit|save|create/i })).toBeNull();
  });

  it("lists each project with its live session (state, branch, cost, Open) or Start session", async () => {
    const fake = seeded();
    fake.sessions = [
      chatSession("S-1", {
        project_id: "P-1",
        project_name: "alpha",
        state: "needs_you",
        branch: "feature-x",
        usage: { input_tokens: 1000, output_tokens: 200, cache_read_tokens: 0, cache_write_tokens: 0, cost_micros: 5 },
      }),
    ];
    await openAgents(fake);

    const alpha = rowOf("alpha");
    expect(alpha.getByText("Needs you")).toBeTruthy();
    expect(alpha.getByText("feature-x")).toBeTruthy();
    expect(alpha.getByText("2.0k wu")).toBeTruthy();
    expect(alpha.getByRole("button", { name: "Open" })).toBeTruthy();
    expect(alpha.queryByRole("button", { name: "Start session" })).toBeNull();

    const beta = rowOf("beta");
    expect(beta.getByText("No live session")).toBeTruthy();
    expect(beta.getByRole("button", { name: "Start session" })).toBeTruthy();
    expect(beta.queryByRole("button", { name: "Open" })).toBeNull();
  });

  it("opens an existing session in a tab with Open, and keeps no tab before that", async () => {
    const fake = seeded();
    fake.sessions = [chatSession("S-1", { project_id: "P-1", project_name: "alpha" })];
    const user = await openAgents(fake);
    expect(screen.queryByRole("tablist")).toBeNull();

    await user.click(rowOf("alpha").getByRole("button", { name: "Open" }));
    expect(await screen.findByRole("tab", { name: "Orchestrator · alpha" })).toBeTruthy();
    expect(screen.getAllByRole("tab")).toHaveLength(1);
  });

  it("starts a session with the project and the Orchestrator, then opens its tab and its stream", async () => {
    const fake = seeded();
    const user = await openAgents(fake);
    await user.click(rowOf("beta").getByRole("button", { name: "Start session" }));

    expect(await screen.findByRole("tab", { name: "Orchestrator · beta" })).toBeTruthy();
    expect(fake.startArgs).toHaveLength(1);
    expect(fake.startArgs[0]).toMatchObject({ project_id: "P-2", agent_id: "orchestrator" });
    expect(fake.startArgs[0]?.command_id).toMatch(/^[0-9a-f-]{16,}$/);
    // The list is read again after the start, and the stream now follows the new chat.
    expect(fake.count("sessionList")).toBe(2);
    await waitFor(() => expect(fake.current()?.options).toEqual({ session: "S-1", after: 0 }));
  });

  it("shows the server's text on the row for a refused start, and starts nothing", async () => {
    const fake = seeded();
    fake.fail("sessionStart", "refused", "project P-2 already has a live Orchestrator, S-4: follow that one", 1);
    const user = await openAgents(fake);
    await user.click(rowOf("beta").getByRole("button", { name: "Start session" }));

    expect(await rowOf("beta").findByText("project P-2 already has a live Orchestrator, S-4: follow that one")).toBeTruthy();
    expect(screen.queryByRole("tab")).toBeNull();
    // A refusal is no disconnect.
    expect(screen.queryByText(/Not connected/)).toBeNull();
    expect(rowOf("beta").getByRole("button", { name: "Start session" })).toBeTruthy();
  });

  it("reuses the command ID when the operator retries a start that ended with unreachable", async () => {
    const fake = seeded();
    fake.fail("sessionStart", "unreachable", "the server did not answer", 1);
    const user = await openAgents(fake);

    await user.click(rowOf("beta").getByRole("button", { name: "Start session" }));
    expect(await rowOf("beta").findByText("the server did not answer")).toBeTruthy();
    expect(fake.startArgs).toHaveLength(1);

    await user.click(rowOf("beta").getByRole("button", { name: "Start session" }));
    expect(await screen.findByRole("tab", { name: "Orchestrator · beta" })).toBeTruthy();
    expect(fake.startArgs).toHaveLength(2);
    expect(fake.startArgs[1]?.command_id).toBe(fake.startArgs[0]?.command_id);
  });

  it("says so when the server lists no agent", async () => {
    const fake = seeded();
    fake.agents = [];
    await openAgentsPage(fake);
    expect(await within(screen.getByRole("main")).findByText("This server lists no agent.")).toBeTruthy();
  });
});

describe("the Agents page makes no call by itself", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("calls nothing after it has opened, for an hour, blurred or focused", async () => {
    const fake = seeded();
    render(<App transport={fake} />);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    fireEvent.click(screen.getByRole("button", { name: "Agents" }));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(0);
    });
    expect(within(screen.getByRole("main")).getByRole("heading", { name: "Agents" })).toBeTruthy();
    const settled = [...fake.calls];
    // The shell's sidebar read the run list once when it mounted on Projects.
    expect(settled).toEqual(["paired", "runList", "projectList", "openStream", "projectList", "sessionList"]);

    for (let i = 0; i < 2; i++) {
      await act(async () => {
        await vi.advanceTimersByTimeAsync(30 * 60 * 1000);
        window.dispatchEvent(new Event("blur"));
        document.dispatchEvent(new Event("visibilitychange"));
        window.dispatchEvent(new Event("focus"));
        window.dispatchEvent(new Event("online"));
      });
    }
    expect(fake.calls).toEqual(settled);
  });
});
