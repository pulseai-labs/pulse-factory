// AC-1 (r1.s4.w2), updated by r1.s5.w4 (11g): the Agents rail item, the agent list in the sidebar, the agent's page
// (identity, Brain, Skills, Permissions, Notifications, Sessions by project) and Start session.
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../App";
import { resetAgentSelection } from "../screens/agentSelection";
import { FakeTransport, chatSession, claudeRuntime, orchestrator, project } from "./fake-transport";

beforeEach(() => {
  resetAgentSelection();
});

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

  it("shows the agent's identity, Brain tiles with display names, Skills, Permissions and Notifications, read only", async () => {
    const fake = seeded();
    fake.agents = [
      orchestrator({ effort: "xhigh", model: "claude-opus-5-5", skills: ["pulse-factory", "review"], permission_mode: "manual" }),
    ];
    await openAgents(fake);

    const main = within(screen.getByRole("main"));
    expect(main.getByRole("heading", { level: 2, name: "Orchestrator" })).toBeTruthy();
    expect(main.getByText("Talks to a project's team")).toBeTruthy();
    // Persona: the record carries no text, and Edit is not built.
    expect(main.getByText("Persona")).toBeTruthy();
    expect(main.getByText("The persona text is not shown in this release.")).toBeTruthy();
    expect(main.getByText("Edit").closest("[data-not-built]")).not.toBeNull();
    // Brain: three tiles, with display names (G3).
    expect(within(main.getByRole("group", { name: "Runtime" })).getByText("claude-code")).toBeTruthy();
    expect(within(main.getByRole("group", { name: "Model" })).getByText("Opus 5.5")).toBeTruthy();
    const effort = within(main.getByRole("group", { name: "Reasoning effort" }));
    expect(effort.getByText("XHigh")).toBeTruthy();
    expect(main.getByRole("group", { name: "Reasoning effort" }).querySelectorAll("[data-bar]")).toHaveLength(4);
    expect(main.getByRole("group", { name: "Reasoning effort" }).querySelectorAll("[data-bar=on]")).toHaveLength(4);
    // Runtime options stay as a quiet line (G8).
    expect(main.getByText("artifacts: off")).toBeTruthy();
    // Skills: one chip each, then Add skill, not built.
    expect(main.getByText("pulse-factory")).toBeTruthy();
    expect(main.getByText("review")).toBeTruthy();
    expect(main.getByText("Add skill").closest("[data-not-built]")).not.toBeNull();
    // Permissions: the actual mode as its value, and the three-way switch, disabled, with the Later tag.
    expect(main.getByText("Ask before edits", { selector: ".ag-perm-value" })).toBeTruthy();
    const switchGroup = main.getByRole("group", { name: "Permissions" });
    const positions = within(switchGroup).getAllByRole("button");
    expect(positions.map((b) => b.textContent)).toEqual(["Read only", "Ask before edits", "Edit freely"]);
    for (const b of positions) {
      expect((b as HTMLButtonElement).disabled).toBe(true);
      expect(b.getAttribute("aria-pressed")).toBe("false");
    }
    expect(within(switchGroup).getByText("Later release")).toBeTruthy();
    // Notifications: the row, not built.
    expect(main.getByText("Send its questions to my phone").closest("[data-not-built]")?.getAttribute("data-not-built")).toBe("row");
    // P1: no edit control in Release 1.
    expect(main.queryByRole("textbox")).toBeNull();
    expect(main.queryByRole("combobox")).toBeNull();
    // The only buttons with such a name are the permission switch's disabled positions ("Edit freely").
    expect(main.queryAllByRole("button", { name: /edit|save|create/i }).filter((b) => !(b as HTMLButtonElement).disabled)).toEqual([]);
  });

  it("fills the effort bars to the effort's rank", async () => {
    const fake = seeded();
    fake.agents = [orchestrator({ effort: "medium" })];
    await openAgents(fake);
    const bars = screen.getByRole("group", { name: "Reasoning effort" }).querySelectorAll("[data-bar=on]");
    expect(bars).toHaveLength(2);
  });

  it("shows an id the display names do not know as the id", async () => {
    const fake = seeded();
    fake.agents = [orchestrator({ model: "fake-model", effort: "turbo", permission_mode: "bypass" })];
    await openAgents(fake);
    expect(within(screen.getByRole("group", { name: "Model" })).getByText("fake-model")).toBeTruthy();
    expect(within(screen.getByRole("group", { name: "Reasoning effort" })).getByText("turbo")).toBeTruthy();
    expect(screen.getByText("bypass", { selector: ".ag-perm-value" })).toBeTruthy();
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
    expect(alpha.getByText("Waiting for your answer")).toBeTruthy();
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

  it("lists only the agents the server lists in the sidebar, the first one selected, and follows a click", async () => {
    const fake = seeded();
    fake.agents = [orchestrator(), orchestrator({ id: "autopilot", name: "Autopilot", title: "Runs a spine for you", model: "claude-sonnet-5-5", effort: "low" })];
    fake.sessions = [
      chatSession("S-1", { project_id: "P-1", project_name: "alpha", state: "needs_you" }),
      chatSession("S-2", { project_id: "P-2", project_name: "beta", agent_id: "autopilot", state: "needs_you" }),
      chatSession("S-3", { project_id: "P-1", project_name: "alpha", agent_id: "autopilot", state: "working" }),
    ];
    const user = await openAgents(fake);
    const side = within(screen.getByRole("region", { name: "Sidebar" }));
    expect(side.getByText("Agents", { selector: ".sb-title" })).toBeTruthy();
    expect(side.getByText("New agent").closest("[data-not-built]")).not.toBeNull();
    const rows = side.getAllByRole("button", { name: /^(Orchestrator|Autopilot)/ });
    expect(rows.map((r) => r.getAttribute("aria-label"))).toEqual(["Orchestrator", "Autopilot"]);
    expect(rows[0]?.getAttribute("aria-current")).toBe("true");
    expect(rows[1]?.getAttribute("aria-current")).toBeNull();
    // An amber count of the agent's live sessions that need you.
    expect(rows[0]?.querySelector(".ui-count")?.textContent).toBe("1");
    expect(rows[1]?.querySelector(".ui-count")?.textContent).toBe("1");
    expect(within(screen.getByRole("main")).getByRole("heading", { level: 2, name: "Orchestrator" })).toBeTruthy();

    await user.click(rows[1] as HTMLElement);
    expect(rows[1]?.getAttribute("aria-current")).toBe("true");
    expect(rows[0]?.getAttribute("aria-current")).toBeNull();
    expect(await within(screen.getByRole("main")).findByRole("heading", { level: 2, name: "Autopilot" })).toBeTruthy();
    expect(within(screen.getByRole("group", { name: "Model" })).getByText("Sonnet 5.5")).toBeTruthy();
    expect(within(screen.getByRole("group", { name: "Reasoning effort" })).getByText("Low")).toBeTruthy();
  });

  it("shows the list with one agent too", async () => {
    await openAgents(seeded());
    const side = within(screen.getByRole("region", { name: "Sidebar" }));
    expect(side.getByRole("button", { name: "Orchestrator" }).getAttribute("aria-current")).toBe("true");
  });

  it("keeps the selection when the page closes and opens again, and starts on the first agent", async () => {
    const fake = seeded();
    fake.agents = [orchestrator(), orchestrator({ id: "autopilot", name: "Autopilot", title: "Runs a spine for you" })];
    const user = await openAgents(fake);
    await user.click(within(screen.getByRole("region", { name: "Sidebar" })).getByRole("button", { name: "Autopilot" }));
    await user.click(within(screen.getByRole("navigation")).getByRole("button", { name: "Projects" }));
    await user.click(within(screen.getByRole("navigation")).getByRole("button", { name: "Agents" }));
    expect(await within(screen.getByRole("main")).findByRole("heading", { level: 2, name: "Autopilot" })).toBeTruthy();
    expect(Object.keys(window.localStorage)).not.toContain("pf.agent");
  });

  it("makes no call when an agent is selected", async () => {
    const fake = seeded();
    fake.agents = [orchestrator(), orchestrator({ id: "autopilot", name: "Autopilot", title: "t" })];
    const user = await openAgents(fake);
    const before = fake.calls.length;
    await user.click(within(screen.getByRole("region", { name: "Sidebar" })).getByRole("button", { name: "Autopilot" }));
    expect(fake.calls.length).toBe(before);
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
    expect(within(screen.getByRole("main")).getByText("Sessions by project")).toBeTruthy();
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
