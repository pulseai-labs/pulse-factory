// AC-1 (r1.s6.w3): the Agents page, its title-bar tab and its sidebar list close the gaps of the second walk
// (W1-29, NG-19, NG-20, NG-21, d17-10, d17-12, d17-15, d17-17, KL-5). The tests assert rendered text and
// markup, as 11g draws it; the pages' calls are in agents.test.tsx.
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";
import { App } from "../App";
import { resetAgentSelection } from "../screens/agentSelection";
import { FakeTransport, claudeRuntime, orchestrator, project } from "./fake-transport";

beforeEach(() => {
  resetAgentSelection();
});

function seeded(): FakeTransport {
  const fake = new FakeTransport({ projects: [project(1, "alpha"), project(2, "beta")] });
  fake.runtimes = [claudeRuntime()];
  fake.agents = [orchestrator()];
  return fake;
}

async function openAgents(fake: FakeTransport) {
  const user = userEvent.setup();
  render(<App transport={fake} />);
  await screen.findByRole("navigation");
  await waitFor(() => expect(fake.count("projectList")).toBe(1));
  await user.click(within(screen.getByRole("navigation")).getByRole("button", { name: "Agents" }));
  await waitFor(() => expect(fake.count("sessionList")).toBe(1));
  return user;
}

const bar = () => within(screen.getByRole("banner"));
const main = () => within(screen.getByRole("main"));
const side = () => within(screen.getByRole("region", { name: "Sidebar" }));

describe("the Agents tab (W1-29)", () => {
  it("reads '<name> agent' with the agent's avatar, and its × reads 'Close <name> agent'", async () => {
    await openAgents(seeded());
    const tab = await bar().findByRole("heading", { level: 1, name: "Orchestrator agent" });
    expect(tab.textContent).toBe("Orchestrator agent");
    expect(tab.querySelector(".ui-avatar[data-agent='orchestrator']")).not.toBeNull();
    expect(tab.querySelector("[data-icon='bot']")).toBeNull();
    expect(bar().getByRole("button", { name: "Close Orchestrator agent" })).toBeTruthy();
    expect(bar().queryByRole("button", { name: "Close Agents" })).toBeNull();
  });

  it("follows the agent the list selects", async () => {
    const fake = seeded();
    fake.agents = [orchestrator(), orchestrator({ id: "autopilot", name: "Autopilot", title: "Runs a spine for you" })];
    const user = await openAgents(fake);
    await bar().findByRole("heading", { level: 1, name: "Orchestrator agent" });
    await user.click(side().getByRole("button", { name: "Autopilot" }));
    const tab = await bar().findByRole("heading", { level: 1, name: "Autopilot agent" });
    expect(tab.querySelector(".ui-avatar")).not.toBeNull();
    expect(bar().getByRole("button", { name: "Close Autopilot agent" })).toBeTruthy();
  });

  it("reads 'Agents' with the bot icon while the server lists no agent", async () => {
    const fake = seeded();
    fake.agents = [];
    await openAgents(fake);
    await main().findByText("This server lists no agent.");
    const tab = bar().getByRole("heading", { level: 1 });
    expect(tab.textContent).toBe("Agents");
    expect(tab.querySelector("[data-icon='bot']")).not.toBeNull();
    expect(tab.querySelector(".ui-avatar")).toBeNull();
    expect(bar().getByRole("button", { name: "Close Agents" })).toBeTruthy();
  });
});

describe("the agent list and the header (NG-19, NG-21, d17-10)", () => {
  it("shows the 'Later release' tag beside New agent, which stays not built", async () => {
    await openAgents(seeded());
    const head = side().getByText("Agents", { selector: ".sb-title" }).parentElement as HTMLElement;
    expect(within(head).getByText("New agent").closest("[data-not-built]")).not.toBeNull();
    const tags = [...head.querySelectorAll(".ui-later")].filter((el) => !el.classList.contains("ui-sr-only") && el.textContent === "Later release");
    expect(tags).toHaveLength(1);
    expect(tags[0]?.closest("[data-not-built]")).toBeNull();
  });

  it("reads 'One session per project' in the Orchestrator's row and in its header chip", async () => {
    await openAgents(seeded());
    const row = await side().findByRole("button", { name: "Orchestrator" });
    expect(within(row).getByText("One session per project")).toBeTruthy();
    expect(within(row).queryByText("Talks to a project's team")).toBeNull();
    const chip = main().getByText("One session per project", { selector: ".ag-chip" });
    expect(chip.querySelector("[data-icon='layers']")).not.toBeNull();
  });

  it("shows another agent's title in its row, and its header has no chip", async () => {
    const fake = seeded();
    fake.agents = [orchestrator(), orchestrator({ id: "autopilot", name: "Autopilot", title: "Runs a spine for you" })];
    const user = await openAgents(fake);
    const row = await side().findByRole("button", { name: "Autopilot" });
    expect(within(row).getByText("Runs a spine for you")).toBeTruthy();
    expect(within(row).queryByText("One session per project")).toBeNull();
    await user.click(row);
    await main().findByRole("heading", { level: 2, name: "Autopilot" });
    expect(main().queryByText("One session per project")).toBeNull();
    expect(document.querySelector(".ag-chip")).toBeNull();
  });

  it("keeps the header's subtitle as the agent's title (P8)", async () => {
    const fake = seeded();
    fake.agents = [orchestrator({ title: "Runs this project" })];
    await openAgents(fake);
    const heading = await main().findByRole("heading", { level: 2, name: "Orchestrator" });
    expect(heading.parentElement?.querySelector(".ag-title")?.textContent).toBe("Runs this project");
    expect(main().queryByText("Plans spines with you and arms workflows")).toBeNull();
  });
});

describe("Brain (d17-12, d17-17)", () => {
  it("shows 'Claude Code' with 'or OMP' under it when both runtimes are listed", async () => {
    const fake = seeded();
    fake.runtimes = [claudeRuntime(), claudeRuntime([], { id: "omp" })];
    await openAgents(fake);
    const tile = within(await main().findByRole("group", { name: "Runtime" }));
    expect(tile.getByText("Claude Code")).toBeTruthy();
    expect(tile.getByText("or OMP", { selector: ".ag-tile-note" })).toBeTruthy();
    expect(tile.queryByText("claude-code")).toBeNull();
  });

  it("shows no note under Runtime when one runtime is listed", async () => {
    await openAgents(seeded());
    const group = await main().findByRole("group", { name: "Runtime" });
    expect(within(group).getByText("Claude Code")).toBeTruthy();
    expect(group.querySelector(".ag-tile-note")).toBeNull();
  });

  it("shows an id the table does not know as it is, and names the other runtimes by their ids", async () => {
    const fake = seeded();
    fake.agents = [orchestrator({ runtime: "mystery" })];
    fake.runtimes = [claudeRuntime([], { id: "mystery" }), claudeRuntime(), claudeRuntime([], { id: "omp" })];
    await openAgents(fake);
    const tile = within(await main().findByRole("group", { name: "Runtime" }));
    expect(tile.getByText("mystery")).toBeTruthy();
    expect(tile.getByText("or Claude Code, OMP", { selector: ".ag-tile-note" })).toBeTruthy();
  });

  it("notes the Model with the agent's billing when it sets one", async () => {
    const fake = seeded();
    fake.agents = [orchestrator({ model: "claude-opus-5-5", runtime_options: { billing: "Team plan" } })];
    await openAgents(fake);
    const tile = within(await main().findByRole("group", { name: "Model" }));
    expect(tile.getByText("Team plan")).toBeTruthy();
    expect(tile.queryByText("Anthropic subscription · wu")).toBeNull();
  });

  it("notes the Model 'Anthropic subscription · wu' for the claude-code runtime, and nothing for another", async () => {
    const fake = seeded();
    fake.agents = [orchestrator({ model: "claude-opus-5-5" })];
    await openAgents(fake);
    expect(within(await main().findByRole("group", { name: "Model" })).getByText("Anthropic subscription · wu")).toBeTruthy();
  });

  it("shows no Model note for another runtime with no billing", async () => {
    const fake = seeded();
    fake.agents = [orchestrator({ runtime: "omp", model: "claude-opus-5-5" })];
    fake.runtimes = [claudeRuntime(), claudeRuntime([], { id: "omp" })];
    await openAgents(fake);
    const group = await main().findByRole("group", { name: "Model" });
    expect(group.querySelector(".ag-tile-note")).toBeNull();
  });

  it("shows no 'Runtime options' line, and keeps the options in the Runtime tile's title", async () => {
    const fake = seeded();
    fake.agents = [orchestrator({ runtime_options: { artifacts: "on", billing: "Team plan" } })];
    await openAgents(fake);
    const tile = await main().findByRole("group", { name: "Runtime" });
    expect(main().queryByText(/Runtime options/i)).toBeNull();
    expect(main().queryByText("artifacts: on", { exact: false })).toBeNull();
    expect(document.querySelector(".ag-options")).toBeNull();
    expect(tile.getAttribute("title")).toBe("artifacts: on, billing: Team plan");
  });

  it("gives the Runtime tile no title when the agent sets no option", async () => {
    const fake = seeded();
    fake.agents = [orchestrator({ runtime_options: {} })];
    await openAgents(fake);
    const tile = await main().findByRole("group", { name: "Runtime" });
    expect(tile.hasAttribute("title")).toBe(false);
  });
});

describe("Permissions (d17-15, P7)", () => {
  const POSITIONS = ["Read only", "Ask before edits", "Edit freely"];

  it.each([
    ["plan", "Read only", "Plan mode"],
    ["manual", "Ask before edits", "Ask before edits"],
    ["acceptEdits", "Edit freely", "Edit automatically"],
    ["auto", "Edit freely", "Auto"],
    ["dontAsk", "Edit freely", "Don't ask"],
    ["bypassPermissions", "Edit freely", "Bypass permissions"],
  ])("mode %s selects %s, and the control's title names %s", async (mode, position, exact) => {
    const fake = seeded();
    fake.agents = [orchestrator({ permission_mode: mode })];
    await openAgents(fake);
    const group = await main().findByRole("group", { name: "Permissions" });
    const buttons = within(group).getAllByRole("button") as HTMLButtonElement[];
    expect(buttons.map((b) => b.textContent)).toEqual(POSITIONS);
    expect(buttons.filter((b) => b.getAttribute("aria-pressed") === "true").map((b) => b.textContent)).toEqual([position]);
    for (const b of buttons) expect(b.disabled).toBe(true);
    expect(group.getAttribute("title")).toBe(exact);
    const hidden = [...group.querySelectorAll(".ui-sr-only")].find((el) => el.textContent === exact);
    expect(hidden).toBeTruthy();
    expect(within(group).getByText("Later release")).toBeTruthy();
  });

  it("selects no position for a mode the table does not know, and names it as it is", async () => {
    const fake = seeded();
    fake.agents = [orchestrator({ permission_mode: "bypass" })];
    await openAgents(fake);
    const group = await main().findByRole("group", { name: "Permissions" });
    const buttons = within(group).getAllByRole("button");
    expect(buttons.filter((b) => b.getAttribute("aria-pressed") === "true")).toEqual([]);
    expect(group.getAttribute("title")).toBe("bypass");
  });

  it("shows no bold mode text above the control", async () => {
    const fake = seeded();
    fake.agents = [orchestrator({ permission_mode: "manual" })];
    await openAgents(fake);
    await main().findByRole("group", { name: "Permissions" });
    expect(document.querySelector(".ag-perm-value")).toBeNull();
  });
});

describe("Notifications (NG-20)", () => {
  it("draws a card with the amber icon tile, the label and description, the tag, then the disabled switch", async () => {
    const fake = seeded();
    const user = await openAgents(fake);
    const label = await main().findByText("Send its questions to my phone");
    const card = label.closest(".ag-notify") as HTMLElement;
    expect(card).not.toBeNull();
    expect(card.getAttribute("aria-disabled")).toBe("true");
    expect(card.hasAttribute("data-not-built")).toBe(true);
    expect(card.querySelector(".ag-notify-tile [data-icon='chat']")).not.toBeNull();
    expect(within(card).getByText("As push notifications with answer buttons")).toBeTruthy();
    const tag = card.querySelector(".ui-later") as HTMLElement;
    const toggle = card.querySelector(".ag-notify-switch") as HTMLElement;
    expect(tag.textContent).toBe("Later release");
    expect(toggle).not.toBeNull();
    // The tag comes before the switch.
    expect(tag.compareDocumentPosition(toggle) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    // Not built: it takes no focus and makes no call.
    expect(card.querySelector("button, input, a[href], [tabindex]")).toBeNull();
    const before = [...fake.calls];
    await user.click(card);
    await user.click(toggle);
    expect(fake.calls).toEqual(before);
  });

  it("does not use the row form of NotBuilt", async () => {
    await openAgents(seeded());
    await main().findByText("Send its questions to my phone");
    expect(document.querySelector("[data-not-built='row']")).toBeNull();
  });
});

describe("the footnote (KL-5)", () => {
  const NOTE = "At most one live Orchestrator session per project. Opening it from a project, from here, or from Needs you lands in the same session.";

  it("shows under Sessions by project for the Orchestrator", async () => {
    await openAgents(seeded());
    const note = await main().findByText(NOTE);
    expect(note.classList.contains("ag-footnote")).toBe(true);
    const sessions = main().getByRole("list", { name: "Sessions by project" });
    expect(sessions.compareDocumentPosition(note) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("shows for the Orchestrator only", async () => {
    const fake = seeded();
    fake.agents = [orchestrator(), orchestrator({ id: "autopilot", name: "Autopilot", title: "Runs a spine for you" })];
    const user = await openAgents(fake);
    await main().findByText(NOTE);
    await user.click(await side().findByRole("button", { name: "Autopilot" }));
    await main().findByRole("heading", { level: 2, name: "Autopilot" });
    expect(main().queryByText(NOTE)).toBeNull();
  });
});
