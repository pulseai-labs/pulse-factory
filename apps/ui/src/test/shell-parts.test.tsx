// AC-1 of `r1.s5.w2` (Approaches 1 to 10 and 15): the parts of the shell and the screens that live in its
// frame. Each test reads one fact of the spec through the accessibility tree, as an operator's tool would.
import type { ChatSessionRecord } from "@pulse-factory/api-client";
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import { App } from "../App";
import { Column } from "../shell/Column";
import { LaterTag, NotBuilt } from "../ui";
import {
  ADDRESS,
  FakeTransport,
  chatSession,
  claudeRuntime,
  frames,
  mergeAck,
  orchestrator,
  project,
  requestFrames,
  requestSummary,
  runSummary,
} from "./fake-transport";

type User = ReturnType<typeof userEvent.setup>;

afterEach(() => {
  window.localStorage.clear();
});

function seed(sessions: ChatSessionRecord[], projects = [project(1, "alpha"), project(2, "beta")]): FakeTransport {
  const fake = new FakeTransport({ projects });
  fake.runtimes = [claudeRuntime()];
  fake.agents = [orchestrator()];
  fake.sessions = sessions;
  return fake;
}

const s1 = (patch: Partial<ChatSessionRecord> = {}) =>
  chatSession("S-1", { project_id: "P-1", project_name: "alpha", worktree_dir: "/home/dev/alpha", ...patch });
const s2 = (patch: Partial<ChatSessionRecord> = {}) =>
  chatSession("S-2", {
    project_id: "P-2",
    project_name: "beta",
    worktree_id: "W-2",
    worktree_dir: "/home/dev/beta",
    branch: "feature/x",
    ...patch,
  });

async function boot(fake: FakeTransport): Promise<User> {
  const user = userEvent.setup();
  render(<App transport={fake} />);
  await screen.findByRole("navigation");
  await waitFor(() => expect(fake.current()).toBeDefined());
  await act(async () => {
    fake.frame(frames.hello(fake.sessions));
  });
  return user;
}

const bar = () => within(screen.getByRole("banner"));
const rail = () => within(screen.getByRole("navigation"));
const side = () => within(screen.getByRole("region", { name: "Sidebar" }));
const main = () => within(screen.getByRole("main"));
const labelOf = (el: Element): string => el.getAttribute("aria-label") ?? el.textContent?.trim() ?? "";

describe("the title bar (11a, Approach 1)", () => {
  it("is one banner with the chip, the toggle, Back, Forward, the title, the tabs, New tab and the pill, in that order", async () => {
    const user = await boot(seed([s1()]));
    await user.click(await side().findByRole("button", { name: /Orchestrator session in alpha/ }));
    await screen.findByRole("tab", { name: "Orchestrator · alpha" });

    const banner = screen.getByRole("banner");
    expect(banner.hasAttribute("data-tauri-drag-region")).toBe(true);
    const parts = [...banner.querySelectorAll("button, h1, [role=tablist]")].filter(
      (el) => el.matches("[role=tablist]") || !el.closest("[role=tablist]"),
    );
    const order = parts.map((el) => (el.matches("[role=tablist]") ? "tabs" : el.matches("h1") ? "title" : labelOf(el)));
    expect(order).toEqual([ADDRESS, "Show or hide the sidebar", "Back", "Forward", "title", "tabs", "New tab", "0 need you"]);
  });

  it("names the page: Projects on Projects and on a chat page, the page's name elsewhere", async () => {
    const user = await boot(seed([s1()]));
    const title = () => bar().getByRole("heading", { level: 1 }).textContent;
    expect(title()).toBe("Projects");
    await user.click(await side().findByRole("button", { name: /Orchestrator session in alpha/ }));
    expect(title()).toBe("Projects");
    await user.click(rail().getByRole("button", { name: "Agents" }));
    expect(title()).toBe("Agents");
    // The title is no tab and no button.
    expect(bar().queryByRole("button", { name: "Agents" })).toBeNull();
    expect(bar().queryAllByRole("tab")).toHaveLength(1);
  });

  it("opens the server menu from the chip, which names the server and carries the 11a title", async () => {
    const fake = seed([]);
    await boot(fake);
    const chip = bar().getByRole("button", { name: ADDRESS });
    await waitFor(() => expect(chip.getAttribute("title")).toMatch(/^Connected to .+ · \d+ ms$/));
    expect(chip.getAttribute("title")?.startsWith(`Connected to ${ADDRESS} · `)).toBe(true);
    expect(chip.getAttribute("aria-haspopup")).toBe("dialog");
  });

  it("shows New tab, which opens the Projects page", async () => {
    const user = await boot(seed([s1()]));
    await user.click(rail().getByRole("button", { name: "Agents" }));
    await user.click(bar().getByRole("button", { name: "New tab" }));
    expect(bar().getByRole("heading", { level: 1 }).textContent).toBe("Projects");
  });

  it("hides the toggle, the tabs, New tab and the pill on the focused pages, and keeps the chip, Back, Forward and the title", async () => {
    const user = await boot(seed([s1()]));
    await user.click(await side().findByRole("button", { name: /Orchestrator session in alpha/ }));
    for (const name of ["Settings", "Needs you"]) {
      await user.click(rail().getByRole("button", { name }));
      const banner = screen.getByRole("banner");
      const labels = [...banner.querySelectorAll("button, h1, [role=tablist]")].map((el) =>
        el.matches("[role=tablist]") ? "tabs" : el.matches("h1") ? "title" : labelOf(el),
      );
      expect(labels, name).toEqual([ADDRESS, "Back", "Forward", "title"]);
      expect(bar().getByRole("heading", { level: 1 }).textContent).toBe(name);
    }
  });

  it("closes a tab with its button, which is in the accessibility tree on every tab", async () => {
    const user = await boot(seed([s1(), s2()]));
    await user.click(await side().findByRole("button", { name: /Orchestrator session in alpha/ }));
    await user.click(await side().findByRole("button", { name: /Orchestrator session in beta/ }));
    expect(bar().getAllByRole("tab")).toHaveLength(2);
    expect(bar().getByRole("button", { name: "Close Orchestrator · alpha" })).toBeTruthy();
    expect(bar().getByRole("button", { name: "Close Orchestrator · beta" })).toBeTruthy();
    await user.click(bar().getByRole("button", { name: "Close Orchestrator · beta" }));
    expect(bar().getAllByRole("tab")).toHaveLength(1);
    expect(bar().getByRole("heading", { level: 1 }).textContent).toBe("Projects");
  });

  it("shows the need-you count in the pill, and the dock's state with aria-pressed", async () => {
    const fake = seed([]);
    const user = await boot(fake);
    await act(async () => {
      fake.requestFrame(requestFrames.requests([requestSummary("R-1"), requestSummary("R-2")], null));
    });
    const pill = bar().getByTitle("Show or hide Needs you");
    expect(pill.textContent).toContain("2 need you");
    expect(pill.getAttribute("aria-pressed")).toBe("false");
    await user.click(pill);
    expect(pill.getAttribute("aria-pressed")).toBe("true");
  });

  it("while the link is down: the chip's dot and the tab dots turn, and the pill is static with the time", async () => {
    const fake = seed([s1()]);
    const user = await boot(fake);
    await user.click(await side().findByRole("button", { name: /Orchestrator session in alpha/ }));
    const chip = bar().getByRole("button", { name: ADDRESS });
    expect(chip.querySelector(".tb-dot")?.className).toContain("tb-dot-up");

    // The server cannot be reached: every call the Agents page makes fails (a call that gets an answer
    // brings the link back up).
    for (const method of ["projectList", "sessionList", "runList", "status"] as const) fake.fail(method, "unreachable", "no answer");
    // The Agents page reads the project list when it opens: the call fails, and the banner shows.
    await user.click(rail().getByRole("button", { name: "Agents" }));
    await screen.findByText(`Not connected to ${ADDRESS}`);

    expect(bar().getByRole("button", { name: ADDRESS }).getAttribute("title")).toBe("Not connected");
    expect(bar().getByRole("button", { name: ADDRESS }).querySelector(".tb-dot")?.className).toContain("tb-dot-down");
    expect(bar().getByRole("tab", { name: "Orchestrator · alpha" }).parentElement?.querySelector(".tb-tab-dot")?.className).toContain(
      "tb-tab-dot-down",
    );
    const pill = bar().getByTitle(/^Counts as of \d\d:\d\d$/);
    expect(pill.tagName).not.toBe("BUTTON");
    expect(pill.textContent).toMatch(/^0 need you · \d\d:\d\d$/);
    expect(bar().queryByTitle("Show or hide Needs you")).toBeNull();
  });

  it("while the link is up and the stream is reconnecting: only the chip's dot turns red; the chip title, the pill and the tab dots keep their live form", async () => {
    const fake = seed([s1()]);
    const user = await boot(fake);
    await user.click(await side().findByRole("button", { name: /Orchestrator session in alpha/ }));
    const chip = () => bar().getByRole("button", { name: ADDRESS });
    await waitFor(() => expect(chip().getAttribute("title")).toMatch(/^Connected to .+ · \d+ ms$/));

    // Only the stream drops. The link stays up, because no call fails.
    await act(async () => {
      fake.emit({ event: "reconnecting", after: 0, retry_in_ms: 1000, reason: "the stream dropped" });
    });
    await screen.findByText(`Not connected to ${ADDRESS}`); // the banner (Approach 7) and the chip dot (Approach 1.1) say it

    expect(chip().querySelector(".tb-dot")?.className).toContain("tb-dot-down");
    // The chip title says "Connected to ..." while the link is up (Approach 1).
    expect(chip().getAttribute("title")).toMatch(/^Connected to .+ · \d+ ms$/);
    // The pill is a button that opens the dock (Approach 1.7), not the static 11i form.
    const pill = bar().getByTitle("Show or hide Needs you");
    expect(pill.tagName).toBe("BUTTON");
    expect(bar().queryByTitle(/^Counts as of/)).toBeNull();
    expect(pill.getAttribute("aria-pressed")).toBe("false");
    await user.click(pill);
    expect(pill.getAttribute("aria-pressed")).toBe("true");
    // The tab dots keep their state colour (Approach 1.5): grey only while the link is down.
    const dot = bar().getByRole("tab", { name: "Orchestrator · alpha" }).parentElement?.querySelector(".tb-tab-dot");
    expect(dot?.className).not.toContain("tb-tab-dot-down");
  });
});

describe("the rail (11a, Approach 3)", () => {
  it("keeps Projects, Needs you, Agents and Settings as buttons, with More and the usage ring as not built", async () => {
    await boot(seed([]));
    const nav = screen.getByRole("navigation");
    expect(nav.getAttribute("aria-label")).toBe("Sections");
    expect(rail().getAllByRole("button").map(labelOf)).toEqual(["Projects", "Needs you", "Agents", "Settings"]);

    const order = [...nav.querySelectorAll("button, [data-not-built]")].map((el) =>
      el.hasAttribute("data-not-built") ? `not built: ${el.textContent}` : labelOf(el),
    );
    expect(order).toEqual(["Projects", "Needs you", "Agents", "not built: MoreLater", "not built: UsageLater", "Settings"]);
    expect(nav.querySelector("[data-icon=layers]")).not.toBeNull();
    expect(nav.querySelector("[data-icon=circle-dot]")).not.toBeNull();
    expect(nav.querySelector("[data-icon=bot]")).not.toBeNull();
    expect(nav.querySelector("[data-icon=more]")).not.toBeNull();
    expect(nav.querySelector("[data-icon=folder]")).toBeNull();
  });

  it("marks Projects on the Projects page and on a chat page, and Agents on Agents", async () => {
    const user = await boot(seed([s1()]));
    const current = () => rail().getAllByRole("button").filter((b) => b.getAttribute("aria-current") !== null).map(labelOf);
    expect(current()).toEqual(["Projects"]);
    await user.click(await side().findByRole("button", { name: /Orchestrator session in alpha/ }));
    expect(current()).toEqual(["Projects"]);
    // On a chat page a click on Projects opens the Projects page; it does not toggle the sidebar.
    await user.click(rail().getByRole("button", { name: "Projects" }));
    expect(bar().getByRole("heading", { level: 1 }).textContent).toBe("Projects");
    expect(screen.queryByRole("region", { name: "Sidebar" })).not.toBeNull();
    await user.click(rail().getByRole("button", { name: "Agents" }));
    expect(current()).toEqual(["Agents"]);
  });

  it("puts the amber count in the Needs you button", async () => {
    const fake = seed([]);
    await boot(fake);
    await act(async () => {
      fake.requestFrame(requestFrames.requests([requestSummary("R-1")], null));
    });
    expect(rail().getByRole("button", { name: "Needs you" }).querySelector(".ui-count")?.textContent).toBe("1");
  });
});

describe("the sidebar (11a, Approach 4)", () => {
  const sessions = () => [
    s1({ state: "needs_you" }),
    chatSession("S-3", { project_id: "P-1", project_name: "alpha", worktree_id: "W-3", branch: "fix/a", state: "idle", created_at: "2026-11-09T11:00:00Z" }),
    s2({ state: "working" }),
  ];

  it("has the head, the By project and By state switch, Search sessions, and per project a count and New session", async () => {
    await boot(seed(sessions()));
    const sidebar = side();
    expect(sidebar.getByText("Projects", { selector: ".sb-title" })).toBeTruthy();
    expect(sidebar.getByRole("button", { name: "By project" }).getAttribute("aria-pressed")).toBe("true");
    expect(sidebar.getByRole("button", { name: "By state" }).getAttribute("aria-pressed")).toBe("false");
    expect(sidebar.getByPlaceholderText("Search sessions")).toBeTruthy();
    // alpha: one session needs you. beta: none does, so no count.
    const alpha = sidebar.getByText("alpha", { exact: true }).closest("li") as HTMLElement;
    expect(within(alpha).getByTitle("1 need you").textContent).toBe("1");
    const beta = sidebar.getByText("beta", { exact: true }).closest("li") as HTMLElement;
    expect(beta.querySelector(".ui-count")).toBeNull();
    expect(sidebar.getByRole("button", { name: "New session in alpha" })).toBeTruthy();
    expect(sidebar.getByRole("button", { name: "New session in beta" })).toBeTruthy();
  });

  it("shows a session row with its accessible name, avatar, branch and a state dot with a title (G8: unqualified stays)", async () => {
    const override: NonNullable<ChatSessionRecord["process"]> = {
      since: "2026-11-09T10:00:00Z",
      runtime_version: "2.1.0",
      qualified: false,
      turn_since: null,
      waiting_since: null,
      tasks: [],
      releases_at: null,
    };
    await boot(seed([s1({ state: "needs_you", process: override })]));
    const row = side().getByRole("button", { name: "Orchestrator session in alpha: Needs you, main, unqualified" });
    expect(row.querySelector(".ui-avatar")).not.toBeNull();
    expect(row.textContent).toContain("Orchestrator");
    expect(row.textContent).toContain("main");
    expect(row.querySelector("[data-icon=git-branch]")).not.toBeNull();
    expect(row.querySelector("[title='Needs you']")).not.toBeNull();
    expect(within(row).getByText("unqualified")).toBeTruthy();
  });

  it("folds a project's rows with its chevron, and does not keep the fold", async () => {
    const user = await boot(seed(sessions()));
    expect(side().getAllByRole("button", { name: /Orchestrator session in alpha/ })).toHaveLength(2);
    await user.click(side().getByRole("button", { name: "Fold alpha" }));
    expect(side().queryAllByRole("button", { name: /Orchestrator session in alpha/ })).toHaveLength(0);
    expect(side().getAllByRole("button", { name: /Orchestrator session in beta/ })).toHaveLength(1);
    expect(Object.keys(window.localStorage)).toEqual(["pf.sidebar"]);
  });

  it("By state groups the rows under Needs you, Working and Idle, leaves out an empty group, and names project and branch", async () => {
    const user = await boot(seed(sessions()));
    await user.click(side().getByRole("button", { name: "By state" }));
    expect(side().getByRole("button", { name: "By state" }).getAttribute("aria-pressed")).toBe("true");
    const groups = [...screen.getByRole("region", { name: "Sidebar" }).querySelectorAll(".sb-group")];
    expect(groups.map((g) => g.querySelector(".sb-group-name")?.textContent)).toEqual(["Needs you", "Working", "Idle"]);
    expect(groups.map((g) => g.querySelector(".sb-group-count")?.textContent)).toEqual(["1", "1", "1"]);
    expect(side().queryByText("Done")).toBeNull();
    expect(groups[1]?.textContent).toContain("beta · feature/x");
    expect(groups[0]?.textContent).toContain("alpha · main");
    // The choice is in the shell's state, not in storage.
    expect(Object.keys(window.localStorage)).toEqual(["pf.sidebar"]);
  });

  it("filters the rows as the operator types, by agent, project and branch, with no call", async () => {
    const fake = seed(sessions());
    const user = await boot(fake);
    const before = fake.calls.length;
    const box = side().getByPlaceholderText("Search sessions");
    await user.type(box, "FEATURE");
    expect(side().getAllByRole("button", { name: /Orchestrator session in/ })).toHaveLength(1);
    expect(side().getByRole("button", { name: /Orchestrator session in beta/ })).toBeTruthy();
    await user.clear(box);
    await user.type(box, "alpha");
    expect(side().getAllByRole("button", { name: /Orchestrator session in alpha/ })).toHaveLength(2);
    expect(side().queryByRole("button", { name: /session in beta/ })).toBeNull();
    await user.clear(box);
    await user.type(box, "orchestr");
    expect(side().getAllByRole("button", { name: /Orchestrator session in/ })).toHaveLength(3);
    expect(fake.calls.length).toBe(before);
  });

  it("lists the workflow runs in one group for the server, as rows that open nothing", async () => {
    const fake = seed([s1()]);
    fake.runs = [
      runSummary("r1", { spine: "r1.s5", state: "active", open_requests: 2, waiting: "you answer R-5" }),
      runSummary("r0", { spine: "r1.s4", state: "ended", end_reason: "the spine merged" }),
      runSummary("r2", { spine: "r2.s1", state: "active", halted_reason: "a unit failed" }),
      runSummary("r3", { spine: "r2.s2", state: "active" }),
    ];
    const user = await boot(fake);
    const group = await screen.findByText("Workflow runs");
    expect(group.className).toContain("eyebrow");
    const row = (id: string) => side().getByLabelText(`Run ${id}`);
    await waitFor(() => expect(row("r1").textContent).toContain("r1.s5"));
    expect(row("r1").textContent).toContain("Waiting: you answer R-5");
    expect(within(row("r1")).getByText("2").className).toContain("ui-count");
    expect(row("r0").textContent).toContain("Ended: the spine merged");
    expect(row("r2").textContent).toContain("Halted: a unit failed");
    expect(row("r3").textContent).toContain("Running");
    expect(row("r1").querySelector("[data-icon=activity]")).not.toBeNull();
    expect(row("r1").querySelector("button")).toBeNull();
    const calls = fake.calls.length;
    await user.click(row("r1"));
    expect(fake.calls.length).toBe(calls);
    expect(bar().getByRole("heading", { level: 1 }).textContent).toBe("Projects");
  });

  it("leaves the Workflow runs group out when the server holds no run", async () => {
    await boot(seed([s1()]));
    await waitFor(() => expect(screen.queryByText("Workflow runs")).toBeNull());
  });

  it("calls runList once when the sidebar mounts, once more when it mounts again, and once when the link comes back", async () => {
    const fake = seed([]);
    const user = await boot(fake);
    await waitFor(() => expect(fake.count("runList")).toBe(1));
    // The sidebar closes and opens again: one more call.
    await user.keyboard("{Meta>}b{/Meta}");
    expect(fake.count("runList")).toBe(1);
    await user.keyboard("{Meta>}b{/Meta}");
    await waitFor(() => expect(fake.count("runList")).toBe(2));
    // A focused page has no sidebar, so no call; coming back to Projects reads the list again.
    await user.click(rail().getByRole("button", { name: "Settings" }));
    expect(fake.count("runList")).toBe(2);
    await user.click(rail().getByRole("button", { name: "Projects" }));
    await waitFor(() => expect(fake.count("runList")).toBe(3));
  });

  it("keeps StreamLine and its Reconnect at the bottom while no chat is followed (G8)", async () => {
    const fake = seed([]);
    await boot(fake);
    await act(async () => {
      fake.emit({ event: "closed", error: { kind: "server", message: "the stream failed" } });
    });
    expect(side().getByText("the stream failed")).toBeTruthy();
    expect(side().getByRole("button", { name: "Reconnect" })).toBeTruthy();
  });

  it("shows only the head 'Agents' in the sidebar of the Agents page (w4 fills the list)", async () => {
    const user = await boot(seed([s1()]));
    await user.click(rail().getByRole("button", { name: "Agents" }));
    expect(side().getByText("Agents", { selector: ".sb-title" })).toBeTruthy();
    expect(side().queryByPlaceholderText("Search sessions")).toBeNull();
    expect(side().queryByText("alpha")).toBeNull();
  });
});

describe("the dock (11a, Approach 5)", () => {
  it("starts closed, opens with the pill, keeps its state in pf.dock and reads it back", async () => {
    const fake = seed([]);
    const user = await boot(fake);
    expect(screen.queryByRole("complementary")).toBeNull();
    // Written when the operator changes it, not on mount: a device that never opened the dock has no key.
    expect(window.localStorage.getItem("pf.dock")).toBeNull();

    await user.click(bar().getByTitle("Show or hide Needs you"));
    const dock = screen.getByRole("complementary", { name: "Needs you" });
    expect(window.localStorage.getItem("pf.dock")).toBe("open");
    expect(within(dock).getByText("Needs you", { selector: ".dock-seg-on" })).toBeTruthy();
    const files = dock.querySelector("[data-not-built=segment]") as HTMLElement;
    expect(files.getAttribute("aria-disabled")).toBe("true");
    expect(files.textContent).toBe("FilesLater release");
    expect(within(dock).getByText("all projects")).toBeTruthy();
    expect(dock.querySelector("[data-icon=circle-dot]")).not.toBeNull();

    document.body.innerHTML = "";
    render(<App transport={fake} />);
    await screen.findByRole("navigation");
    expect(screen.queryByRole("complementary", { name: "Needs you" })).not.toBeNull();

    await user.click(bar().getByTitle("Show or hide Needs you"));
    expect(screen.queryByRole("complementary")).toBeNull();
    expect(window.localStorage.getItem("pf.dock")).toBe("closed");
  });

  it("is on the Projects, Agents and chat pages only", async () => {
    window.localStorage.setItem("pf.dock", "open");
    const user = await boot(seed([s1()]));
    expect(screen.queryByRole("complementary")).not.toBeNull();
    await user.click(rail().getByRole("button", { name: "Agents" }));
    expect(screen.queryByRole("complementary")).not.toBeNull();
    await user.click(rail().getByRole("button", { name: "Settings" }));
    expect(screen.queryByRole("complementary")).toBeNull();
  });

  it("lists the open requests, a merge approval first, with the run, the unit, the age and the first line of the reason", async () => {
    window.localStorage.setItem("pf.dock", "open");
    const fake = seed([]);
    await boot(fake);
    await act(async () => {
      fake.requestFrame(
        requestFrames.requests(
          [
            requestSummary("R-1", { run: "r1-a", unit: "w2", reason: "the implementer asks a question\nsecond line", opened_at: "2026-11-09T10:00:00.000Z" }),
            mergeAck("R-2", { run: "r1-a", opened_at: "2026-11-09T11:00:00.000Z" }),
          ],
          null,
        ),
      );
    });
    const dock = within(screen.getByRole("complementary", { name: "Needs you" }));
    const cards = dock.getAllByRole("listitem");
    expect(cards).toHaveLength(2);
    expect(cards[0]?.textContent).toContain("spine merge waits on the claim's ack");
    expect(cards[1]?.textContent).toContain("r1-a");
    expect(cards[1]?.textContent).toContain("w2");
    expect(cards[1]?.textContent).toContain("the implementer asks a question");
    expect(cards[1]?.textContent).not.toContain("second line");
    expect(cards[1]?.querySelector(".dock-age")?.textContent).toMatch(/\d/);
    for (const card of cards) expect(within(card).getByRole("button", { name: "Open" })).toBeTruthy();
  });

  it("says so when nothing needs the operator, and makes no call", async () => {
    window.localStorage.setItem("pf.dock", "open");
    const fake = seed([]);
    await boot(fake);
    const before = fake.calls.length;
    expect(within(screen.getByRole("complementary")).getByText("Nothing needs you.")).toBeTruthy();
    expect(fake.calls.length).toBe(before);
  });
});

describe("the not-connected banner (11i, Approach 7)", () => {
  it("shows while the stream is reconnecting, names the server, and Retry now reconnects the stream", async () => {
    const fake = seed([]);
    const user = await boot(fake);
    expect(screen.queryByText(/Not connected/)).toBeNull();
    const streams = fake.streams.length;
    await act(async () => {
      fake.emit({ event: "reconnecting", after: 0, retry_in_ms: 1000, reason: "the stream dropped" });
    });
    expect(screen.getByText(`Not connected to ${ADDRESS}`)).toBeTruthy();
    expect(screen.getByText("Your sessions and runs keep going on the server. Reconnecting…")).toBeTruthy();
    expect(screen.getAllByText(/Reconnecting…/).length).toBeGreaterThan(0); // StreamLine says it too
    await user.click(screen.getByRole("button", { name: "Retry now" }));
    await waitFor(() => expect(fake.streams.length).toBe(streams + 1));
    // The stream opens again: the banner goes when the first event arrives.
    await act(async () => {
      fake.frame(frames.hello([]));
    });
    expect(screen.queryByText(/Not connected/)).toBeNull();
  });

  it("is the full-width 11i form: a red dot, the title, the body and the button with its icon", async () => {
    const fake = seed([]);
    fake.fail("projectList", "unreachable", "no answer");
    fake.fail("status", "unreachable", "no answer");
    await boot(fake);
    const banner = (await screen.findByText(`Not connected to ${ADDRESS}`)).closest(".banner") as HTMLElement;
    expect(banner.querySelector(".banner-dot")).not.toBeNull();
    expect(within(banner).getByText(/Retrying in \d+s\./)).toBeTruthy();
    expect(within(banner).getByRole("button", { name: "Retry now" }).querySelector("[data-icon=refresh]")).not.toBeNull();
    // The Add project form is disabled while the server cannot be reached.
    expect((screen.getByLabelText("Project root") as HTMLInputElement).disabled).toBe(true);
    expect((screen.getByRole("button", { name: "Add project" }) as HTMLButtonElement).disabled).toBe(true);
  });
});

describe("the server menu (11h, Approach 6)", () => {
  async function openMenu(fake: FakeTransport): Promise<User> {
    const user = await boot(fake);
    await user.click(bar().getByRole("button", { name: ADDRESS }));
    return user;
  }

  it("shows the server block, the red row, and the four rows of a later release, and closes on Escape and on an outside click", async () => {
    const user = await openMenu(seed([]));
    const menu = within(screen.getByRole("dialog", { name: "Server menu" }));
    expect(menu.getAllByText(ADDRESS).length).toBeGreaterThan(0);
    expect(menu.getByText(/^Connected/)).toBeTruthy();
    expect(menu.getByText("Address")).toBeTruthy();
    expect(menu.getByRole("button", { name: "Disconnect this Mac" })).toBeTruthy();
    expect(menu.getByText("Coming in a later release")).toBeTruthy();
    const later = [...screen.getByRole("dialog").querySelectorAll("[data-not-built=item]")];
    expect(later.map((el) => el.textContent)).toEqual([
      "Add a serverLater release",
      "Pair a phoneLater release",
      "Paired devicesLater release",
      "Server settingsLater release",
    ]);
    for (const el of later) expect(el.getAttribute("aria-disabled")).toBe("true");

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).toBeNull();
    await user.click(bar().getByRole("button", { name: ADDRESS }));
    expect(screen.queryByRole("dialog")).not.toBeNull();
    await user.click(main().getByRole("heading", { name: "Projects" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    // The chip toggles: a second click closes the menu.
    await user.click(bar().getByRole("button", { name: ADDRESS }));
    await user.click(bar().getByRole("button", { name: ADDRESS }));
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("asks before it disconnects: Cancel calls nothing, and Disconnect calls forget once", async () => {
    const fake = seed([]);
    const user = await openMenu(fake);
    const menu = () => within(screen.getByRole("dialog", { name: "Server menu" }));
    await user.click(menu().getByRole("button", { name: "Disconnect this Mac" }));
    const confirm = within(screen.getByRole("alertdialog"));
    expect(confirm.getByText(`Disconnect this Mac from ${ADDRESS}?`)).toBeTruthy();
    expect(confirm.getByText("Sessions and runs keep going on the server. To come back, pair again with a new code.")).toBeTruthy();
    expect(fake.count("forget")).toBe(0);

    await user.click(confirm.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(menu().getByRole("button", { name: "Disconnect this Mac" })).toBeTruthy();
    expect(fake.count("forget")).toBe(0);

    await user.click(menu().getByRole("button", { name: "Disconnect this Mac" }));
    await user.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "Disconnect" }));
    expect(await screen.findByText("Connect to your server")).toBeTruthy();
    expect(fake.count("forget")).toBe(1);
  });

  it("shows the error of a failed disconnect in the menu", async () => {
    const fake = seed([]);
    fake.fail("forget", "server", "could not remove the key");
    const user = await openMenu(fake);
    await user.click(screen.getByRole("button", { name: "Disconnect this Mac" }));
    await user.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "Disconnect" }));
    expect((await within(screen.getByRole("dialog")).findByRole("alert")).textContent).toBe("could not remove the key");
  });
});

describe("the Projects page (11e, Approach 8)", () => {
  it("is a centred column with the head, a block per project, and a grid of session cards", async () => {
    const fake = seed([s1({ state: "needs_you" }), s2({ state: "working" })]);
    fake.projects.push(project(3, "gamma"));
    await boot(fake);
    const page = main();
    expect(page.getByRole("heading", { level: 2, name: "Projects" })).toBeTruthy();
    expect(screen.getByRole("main").querySelector(".column")).not.toBeNull();
    // No subtitle while the link is up.
    expect(page.queryByText(/projects? on /)).toBeNull();
    const alpha = page.getByRole("heading", { level: 3, name: "alpha" }).closest(".pj-block") as HTMLElement;
    expect(within(alpha).getByText("/home/dev/alpha").className).toContain("mono");
    expect(within(alpha).getByTitle("1 need you")).toBeTruthy();
    const card = within(alpha).getByRole("button", { name: "Open Orchestrator session in alpha" });
    expect(card.querySelector(".ui-avatar")).not.toBeNull();
    expect(card.textContent).toContain("Orchestrator");
    expect(card.textContent).toContain("Needs you");
    expect(card.textContent).toContain("main");
    expect(card.textContent).toMatch(/\d+ wu/);
    expect(within(alpha).getByRole("button", { name: "New session in alpha" })).toBeTruthy();
    // A project with no live session shows one quiet line.
    const gamma = page.getByRole("heading", { level: 3, name: "gamma" }).closest(".pj-block") as HTMLElement;
    expect(within(gamma).getByText("No live session")).toBeTruthy();
  });

  it("By state shows the cards in the four groups, each with its project's name", async () => {
    const user = await boot(seed([s1({ state: "needs_you" }), s2({ state: "working" })]));
    await user.click(within(screen.getByRole("main")).getByRole("button", { name: "By state" }));
    const groups = [...screen.getByRole("main").querySelectorAll(".pj-group")];
    expect(groups.map((g) => g.querySelector(".pj-group-name")?.textContent)).toEqual(["Needs you", "Working"]);
    expect(groups[0]?.textContent).toContain("alpha");
    expect(groups[1]?.textContent).toContain("beta");
  });

  it("opens the chat from a card, and shows 'As of HH:MM, when <name> was last reachable' while the link is down", async () => {
    const fake = seed([s1()]);
    const user = await boot(fake);
    await user.click(main().getByRole("button", { name: "Open Orchestrator session in alpha" }));
    expect(await screen.findByRole("tab", { name: "Orchestrator · alpha" })).toBeTruthy();
    await user.click(rail().getByRole("button", { name: "Projects" }));
    fake.fail("projectList", "unreachable", "no answer");
    fake.fail("status", "unreachable", "no answer");
    await user.click(rail().getByRole("button", { name: "Agents" }));
    await user.click(rail().getByRole("button", { name: "Projects" }));
    expect(await main().findByText(new RegExp(`^As of \\d\\d:\\d\\d, when ${ADDRESS.replaceAll(".", "\\.")} was last reachable$`))).toBeTruthy();
  });

  it("keeps the empty state and the Add project form", async () => {
    await boot(seed([], []));
    expect(await main().findByText("No projects yet. Add one below.")).toBeTruthy();
    expect(screen.getByRole("form", { name: "Add project" })).toBeTruthy();
    expect(screen.getByLabelText("Project root")).toBeTruthy();
    expect(screen.getByLabelText("Name (optional)")).toBeTruthy();
  });
});

describe("New session (Approach 8)", () => {
  it("opens the chat of the Orchestrator when the project has a live session, with no call", async () => {
    const fake = seed([s1()]);
    const user = await boot(fake);
    await user.click(side().getByRole("button", { name: "New session in alpha" }));
    expect(await screen.findByRole("tab", { name: "Orchestrator · alpha" })).toBeTruthy();
    expect(fake.count("sessionStart")).toBe(0);
  });

  it("starts a session of the Orchestrator when the project has none, refreshes the list and opens the chat", async () => {
    const fake = seed([s1()]);
    const user = await boot(fake);
    const lists = fake.count("sessionList");
    await user.click(main().getByRole("button", { name: "New session in beta" }));
    expect(await screen.findByRole("tab", { name: "Orchestrator · beta" })).toBeTruthy();
    expect(fake.startArgs).toHaveLength(1);
    expect(fake.startArgs[0]).toMatchObject({ project_id: "P-2", agent_id: "orchestrator" });
    expect(fake.startArgs[0]?.command_id).toMatch(/^[0-9a-f-]{16,}$/);
    expect(fake.count("sessionList")).toBeGreaterThan(lists);
  });

  it("reuses the command ID when a retry follows an unreachable start, and shows an error under the row", async () => {
    const fake = seed([s1()]);
    fake.fail("sessionStart", "unreachable", "no answer", 1);
    const user = await boot(fake);
    await user.click(side().getByRole("button", { name: "New session in beta" }));
    const row = (await side().findByText("no answer")).closest("li") as HTMLElement;
    expect(within(row).getByText("beta")).toBeTruthy();
    expect(fake.startArgs).toHaveLength(1);
    await user.click(side().getByRole("button", { name: "New session in beta" }));
    await screen.findByRole("tab", { name: "Orchestrator · beta" });
    expect(fake.startArgs).toHaveLength(2);
    expect(fake.startArgs[1]?.command_id).toBe(fake.startArgs[0]?.command_id);
  });

  it("shows a refusal as one line under the project's block on the page", async () => {
    const fake = seed([s1()]);
    fake.fail("sessionStart", "refused", "the project root is gone", 1);
    const user = await boot(fake);
    await user.click(main().getByRole("button", { name: "New session in beta" }));
    const block = (await main().findByText("the project root is gone")).closest(".pj-block") as HTMLElement;
    expect(within(block).getByRole("heading", { name: "beta" })).toBeTruthy();
  });
});

describe("NotBuilt (11l, Approach 10)", () => {
  const FORMS = ["item", "button", "rail", "segment", "field", "row"] as const;

  it.each(FORMS)("the %s form is disabled, not focusable, opens nothing and names a later release", async (form) => {
    const user = userEvent.setup();
    const { container } = render(
      <div>
        <button type="button">before</button>
        <NotBuilt as={form} label="Thing" icon="settings" description="A function that is not built" />
        <button type="button">after</button>
      </div>,
    );
    const el = container.querySelector(`[data-not-built=${form}]`) as HTMLElement;
    expect(el).not.toBeNull();
    expect(el.getAttribute("aria-disabled")).toBe("true");
    expect(el.tabIndex).toBe(-1);
    expect(el.querySelector("button, a, input, select, textarea, [tabindex]")).toBeNull();
    expect(el.textContent).toContain(form === "rail" ? "Later" : "Later release");
    expect(el.textContent).not.toMatch(/Release \d/);

    await user.click(el);
    await user.tab();
    expect(document.activeElement?.textContent).toBe("before");
    await user.tab();
    expect(document.activeElement?.textContent).toBe("after");
  });

  it("the row form has its label, its description line, an off switch and the tag", () => {
    const { container } = render(<NotBuilt as="row" label="Notifications" description="Tell me when a session needs me" />);
    const el = container.querySelector("[data-not-built=row]") as HTMLElement;
    expect(el.textContent).toContain("Notifications");
    expect(el.textContent).toContain("Tell me when a session needs me");
    expect(el.querySelector(".nb-switch")).not.toBeNull();
    expect(el.textContent).toContain("Later release");
  });

  it("the field form takes no text", async () => {
    const user = userEvent.setup();
    const { container } = render(<NotBuilt as="field" label="Parked item" />);
    await user.type(container.querySelector("[data-not-built=field]") as HTMLElement, "text");
    expect(container.querySelector("input, textarea")).toBeNull();
    expect(container.textContent).not.toContain("text");
  });

  it("LaterTag says 'Later release', or 'Later' when compact", () => {
    const { container } = render(
      <>
        <LaterTag />
        <LaterTag compact />
      </>,
    );
    const tags = [...container.querySelectorAll(".ui-later")];
    expect(tags.map((t) => t.textContent)).toEqual(["Later release", "Later"]);
    expect(tags[1]?.className).toContain("ui-later-compact");
  });

  it("the Release 1 screens use no locked button", async () => {
    await boot(seed([s1()]));
    expect(document.querySelector(".ui-btn-locked")).toBeNull();
  });
});

describe("the page column (Approach 8)", () => {
  it("is 1180 px by default through the token, and takes another width", () => {
    const { container } = render(
      <>
        <Column>one</Column>
        <Column max={860}>two</Column>
      </>,
    );
    const [a, b] = [...container.querySelectorAll(".column")] as HTMLElement[];
    expect(a?.style.maxWidth).toBe("");
    expect(b?.style.maxWidth).toBe("860px");
    expect(a?.textContent).toBe("one");
  });
});

describe("landmarks (Approach 15)", () => {
  it("has one navigation, the rail, and no button but the rail's names Projects or Settings", async () => {
    const user = await boot(seed([s1()]));
    await user.click(bar().getByRole("button", { name: ADDRESS }));
    await user.click(bar().getByTitle("Show or hide Needs you"));
    expect(screen.getAllByRole("navigation")).toHaveLength(1);
    const outside = screen
      .getAllByRole("button")
      .filter((b) => /projects|settings/i.test(b.getAttribute("aria-label") ?? b.textContent ?? ""))
      .filter((b) => !screen.getByRole("navigation").contains(b));
    expect(outside.map(labelOf)).toEqual([]);

    // The same on a chat page and on Agents.
    await user.keyboard("{Escape}");
    fireEvent.click(side().getByRole("button", { name: /Orchestrator session in alpha/ }));
    const onChat = screen
      .getAllByRole("button")
      .filter((b) => /projects|settings/i.test(b.getAttribute("aria-label") ?? b.textContent ?? ""))
      // The chat header's gear (11a, `r1.s5.w3`) is named "Agent settings": it opens Agents, not Settings.
      .filter((b) => b.getAttribute("aria-label") !== "Agent settings")
      .filter((b) => !screen.getByRole("navigation").contains(b));
    expect(onChat.map(labelOf)).toEqual([]);
  });
});
