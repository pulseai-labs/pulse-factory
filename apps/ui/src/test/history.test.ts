// AC-1 of `r1.s5.w2` (Approach 2): the page history of the shell as pure functions over `{ entries, index }`,
// and the navigation seam that round 3 uses. The seam is read through two components that sit under the
// provider (the Projects page opens a chat, the dock opens a page), so the test needs no JSX.
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { createElement } from "react";
import { describe, expect, it } from "vitest";
import { App } from "../App";
import {
  back,
  canBack,
  canForward,
  closeTab,
  currentPage,
  forward,
  initialHistory,
  push,
  type History,
  type Page,
} from "../shell/history";
import { chatSession, FakeTransport, frames, orchestrator, claudeRuntime, project, requestSummary } from "./fake-transport";

const chat = (session: string): Page => ({ kind: "chat", session });
const PROJECTS: Page = { kind: "projects" };
const AGENTS: Page = { kind: "agents" };
const SETTINGS: Page = { kind: "settings" };

function of(entries: Page[], index: number): History {
  return { entries, index };
}

describe("history: push", () => {
  it("starts on Projects, with nothing behind and nothing ahead", () => {
    const h = initialHistory();
    expect(h).toEqual({ entries: [PROJECTS], index: 0 });
    expect(currentPage(h)).toEqual(PROJECTS);
    expect(canBack(h)).toBe(false);
    expect(canForward(h)).toBe(false);
  });

  it("adds a page after the one on screen", () => {
    const h = push(initialHistory(), AGENTS);
    expect(h).toEqual({ entries: [PROJECTS, AGENTS], index: 1 });
    expect(canBack(h)).toBe(true);
    expect(canForward(h)).toBe(false);
  });

  it("adds no entry for the page that is on screen, and returns the same history", () => {
    const h = push(initialHistory(), AGENTS);
    expect(push(h, AGENTS)).toBe(h);
    const c = push(h, chat("S-1"));
    expect(push(c, chat("S-1"))).toBe(c);
    // Another chat is another page.
    expect(push(c, chat("S-2")).entries).toHaveLength(4);
  });

  it("drops the pages ahead when a new page is pushed from the middle", () => {
    let h = push(push(initialHistory(), AGENTS), SETTINGS);
    h = back(h);
    expect(canForward(h)).toBe(true);
    h = push(h, chat("S-1"));
    expect(h).toEqual({ entries: [PROJECTS, AGENTS, chat("S-1")], index: 2 });
    expect(canForward(h)).toBe(false);
  });
});

describe("history: back and forward", () => {
  it("back at the first entry stays there", () => {
    const h = initialHistory();
    expect(back(h)).toEqual(h);
    expect(canBack(back(h))).toBe(false);
  });

  it("forward at the last entry stays there", () => {
    const h = push(initialHistory(), AGENTS);
    expect(forward(h)).toEqual(h);
    expect(canForward(forward(h))).toBe(false);
  });

  it("moves one entry at a time and keeps every entry", () => {
    const h = push(push(initialHistory(), AGENTS), SETTINGS);
    const b1 = back(h);
    expect(currentPage(b1)).toEqual(AGENTS);
    const b2 = back(b1);
    expect(currentPage(b2)).toEqual(PROJECTS);
    expect(b2.entries).toEqual(h.entries);
    expect(canForward(b2)).toBe(true);
    expect(currentPage(forward(forward(b2)))).toEqual(SETTINGS);
  });
});

describe("history: closing a tab", () => {
  it("takes the pages of that chat out, and the chat on screen goes back to the page before it", () => {
    const h = of([PROJECTS, chat("S-1"), AGENTS, chat("S-1")], 3);
    const closed = closeTab(h, "S-1");
    expect(closed.entries).toEqual([PROJECTS, AGENTS]);
    // The chat was on screen: the page before it (Agents) is on screen now.
    expect(currentPage(closed)).toEqual(AGENTS);
  });

  it("keeps the page on screen when the closed chat is before it", () => {
    const h = of([PROJECTS, chat("S-1"), AGENTS], 2);
    const closed = closeTab(h, "S-1");
    expect(closed).toEqual({ entries: [PROJECTS, AGENTS], index: 1 });
    expect(currentPage(closed)).toEqual(AGENTS);
  });

  it("keeps the page on screen when the closed chat is after it", () => {
    const h = of([PROJECTS, AGENTS, chat("S-1")], 1);
    const closed = closeTab(h, "S-1");
    expect(closed).toEqual({ entries: [PROJECTS, AGENTS], index: 1 });
    expect(canForward(closed)).toBe(false);
  });

  it("closes the chat on screen at the first entries and lands on the first page", () => {
    const h = of([PROJECTS, chat("S-1")], 1);
    expect(closeTab(h, "S-1")).toEqual({ entries: [PROJECTS], index: 0 });
  });

  it("leaves other chats alone", () => {
    const h = of([PROJECTS, chat("S-1"), chat("S-2")], 2);
    const closed = closeTab(h, "S-1");
    expect(closed.entries).toEqual([PROJECTS, chat("S-2")]);
    expect(currentPage(closed)).toEqual(chat("S-2"));
  });
});

describe("the navigation seam", () => {
  function seeded(): FakeTransport {
    const fake = new FakeTransport({ projects: [project(1, "alpha")] });
    fake.runtimes = [claudeRuntime()];
    fake.agents = [orchestrator()];
    fake.sessions = [chatSession("S-1", { project_id: "P-1", project_name: "alpha", state: "idle" })];
    return fake;
  }

  it("openSession opens a chat tab, and Back and Forward follow the history", async () => {
    const fake = seeded();
    const user = userEvent.setup();
    render(createElement(App, { transport: fake }));
    await screen.findByRole("navigation");
    await waitFor(() => expect(fake.current()).toBeDefined());
    const { act } = await import("@testing-library/react");
    await act(async () => {
      fake.frame(frames.hello(fake.sessions));
    });

    const main = within(screen.getByRole("main"));
    await user.click(await main.findByRole("button", { name: /Orchestrator session in alpha/ }));
    const tab = await screen.findByRole("tab", { name: "Orchestrator · alpha" });
    expect(tab.getAttribute("aria-selected")).toBe("true");
    const bar = within(screen.getByRole("banner"));
    expect((bar.getByRole("button", { name: "Back" }) as HTMLButtonElement).disabled).toBe(false);

    await user.click(bar.getByRole("button", { name: "Back" }));
    expect(bar.getByRole("heading", { level: 1 }).textContent).toBe("Projects");
    expect(screen.getByRole("tab", { name: "Orchestrator · alpha" }).getAttribute("aria-selected")).toBe("false");
    expect((bar.getByRole("button", { name: "Forward" }) as HTMLButtonElement).disabled).toBe(false);

    await user.click(bar.getByRole("button", { name: "Forward" }));
    expect(screen.getByRole("tab", { name: "Orchestrator · alpha" }).getAttribute("aria-selected")).toBe("true");
  });

  it("openPage opens a page: the dock's Open button goes to Needs you, and Back returns", async () => {
    const fake = seeded();
    const user = userEvent.setup();
    render(createElement(App, { transport: fake }));
    await screen.findByRole("navigation");
    await waitFor(() => expect(fake.current()).toBeDefined());
    const { act } = await import("@testing-library/react");
    await act(async () => {
      fake.requestFrame({ type: "requests", requests: [requestSummary("R-1")], claim: null });
    });

    const bar = within(screen.getByRole("banner"));
    await user.click(bar.getByTitle("Show or hide Needs you"));
    const dock = within(screen.getByRole("complementary", { name: "Needs you" }));
    await user.click(dock.getByRole("button", { name: "Open" }));

    expect(within(screen.getByRole("banner")).getByRole("heading", { level: 1 }).textContent).toBe("Needs you");
    // Needs you is a focused page: no sidebar, no dock.
    expect(screen.queryByRole("region", { name: "Sidebar" })).toBeNull();
    expect(screen.queryByRole("complementary")).toBeNull();

    await user.click(within(screen.getByRole("banner")).getByRole("button", { name: "Back" }));
    expect(within(screen.getByRole("banner")).getByRole("heading", { level: 1 }).textContent).toBe("Projects");
  });
});
