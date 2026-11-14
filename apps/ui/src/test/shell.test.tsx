import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { App } from "../App";
import { alpha, bootChat, seeded } from "./chat-helpers";
import { ADDRESS, FakeTransport, chatSession, device, project } from "./fake-transport";

async function open(fake: FakeTransport) {
  const user = userEvent.setup();
  const view = render(<App transport={fake} />);
  await screen.findByRole("navigation");
  await waitFor(() => expect(fake.count("projectList")).toBe(1));
  return { user, view };
}

const rail = () => within(screen.getByRole("navigation"));
const titlebar = () => within(screen.getByRole("banner"));
const sidebar = () => screen.queryByRole("region", { name: "Sidebar" });
const LATER = ["Add a server", "Pair a phone", "Paired devices", "Server settings"];

describe("the shell (2a to 2c)", () => {
  it("has a rail with Projects, Needs you, Agents and Settings as buttons, and More and the usage ring as not built", async () => {
    await open(new FakeTransport());
    expect(rail().getAllByRole("button").map((b) => b.getAttribute("aria-label"))).toEqual([
      "Projects",
      "Needs you",
      "Agents",
      "Settings",
    ]);
    // r1.s4.w3: no badge while no request is open.
    expect(rail().getByRole("button", { name: "Needs you" }).querySelector("[title$=' open']")).toBeNull();
    // 11a and 11l: More and the usage ring are in the rail, dimmed, with the "Later" tag. They are not buttons.
    for (const later of ["More", "Usage"]) {
      const item = rail().getByText(later).closest("[data-not-built]");
      expect(item?.getAttribute("aria-disabled")).toBe("true");
      expect(item?.getAttribute("data-not-built")).toBe("rail");
      expect(rail().queryByRole("button", { name: later })).toBeNull();
    }
    expect(rail().getAllByText("Later")).toHaveLength(2);
    // No request is open: the pill in the title bar says so.
    expect(titlebar().getByText("0 need you")).toBeTruthy();
    // No right panel, and no tab strip until a chat opens.
    expect(screen.queryByRole("complementary")).toBeNull();
    expect(screen.queryAllByRole("tab")).toHaveLength(0);
    expect(screen.queryByRole("tablist")).toBeNull();
  });

  it("toggles the sidebar with ⌘B and with a click on the selected Projects icon", async () => {
    const { user } = await open(new FakeTransport({ projects: [project(1, "alpha")] }));
    expect(sidebar()).not.toBeNull();

    await user.keyboard("{Meta>}b{/Meta}");
    expect(sidebar()).toBeNull();
    await user.keyboard("{Meta>}b{/Meta}");
    expect(sidebar()).not.toBeNull();

    const projects = rail().getByRole("button", { name: "Projects" });
    expect(projects.getAttribute("title")).toMatch(/⌘B/);
    await user.click(projects);
    expect(sidebar()).toBeNull();
    expect(projects.getAttribute("title")).toMatch(/show the sidebar/);
    await user.click(projects);
    expect(sidebar()).not.toBeNull();
    expect(projects.getAttribute("title")).toMatch(/hide the sidebar/);
  });

  it("keeps only the sidebar's and the dock's open state in localStorage, and reads the sidebar back", async () => {
    const fake = new FakeTransport();
    const { user, view } = await open(fake);
    // The dock is written when the operator changes it, not on mount.
    expect(Object.keys(window.localStorage)).toEqual(["pf.sidebar"]);
    expect(window.localStorage.getItem("pf.sidebar")).toBe("open");

    await user.keyboard("{Meta>}b{/Meta}");
    expect(Object.keys(window.localStorage)).toEqual(["pf.sidebar"]);
    expect(window.localStorage.getItem("pf.sidebar")).toBe("closed");
    await user.click(titlebar().getByTitle("Show or hide Needs you"));
    expect(Object.keys(window.localStorage).sort()).toEqual(["pf.dock", "pf.sidebar"]);
    expect(window.localStorage.getItem("pf.dock")).toBe("open");

    view.unmount();
    render(<App transport={fake} />);
    await screen.findByRole("navigation");
    expect(sidebar()).toBeNull();
  });

  it("moves through the page history with back and forward, and the title bar shows the page", async () => {
    const { user } = await open(new FakeTransport());
    expect(titlebar().getByText("Projects")).toBeTruthy();
    expect((titlebar().getByRole("button", { name: "Back" }) as HTMLButtonElement).disabled).toBe(true);
    expect((titlebar().getByRole("button", { name: "Forward" }) as HTMLButtonElement).disabled).toBe(true);

    await user.click(rail().getByRole("button", { name: "Settings" }));
    expect(titlebar().getByText("Settings")).toBeTruthy();
    // 11k (r1.s6.w1): the Projects tab stays beside the Settings tab.
    expect(titlebar().getByRole("button", { name: "Projects" })).toBeTruthy();
    // Settings has no sidebar.
    expect(sidebar()).toBeNull();

    await user.click(titlebar().getByRole("button", { name: "Back" }));
    expect(titlebar().getByText("Projects")).toBeTruthy();
    expect((titlebar().getByRole("button", { name: "Forward" }) as HTMLButtonElement).disabled).toBe(false);

    await user.click(titlebar().getByRole("button", { name: "Forward" }));
    expect(titlebar().getByText("Settings")).toBeTruthy();
    expect((titlebar().getByRole("button", { name: "Forward" }) as HTMLButtonElement).disabled).toBe(true);
  });

  it("opens the server menu from the chip in the title bar and shows what this spine builds", async () => {
    const { user } = await open(new FakeTransport());
    expect(screen.queryByRole("dialog")).toBeNull();
    // The chip's accessible name is the server's name, else its address (the fake has no name).
    await user.click(titlebar().getByRole("button", { name: new RegExp(ADDRESS.replaceAll(".", "\\.")) }));

    const menu = within(screen.getByRole("dialog", { name: "Server menu" }));
    expect(menu.getAllByText(ADDRESS).length).toBeGreaterThan(0);
    expect(menu.getByText("Connected")).toBeTruthy();
    expect(menu.getByText(/^\d+ ms$/)).toBeTruthy();
    expect(menu.getByRole("button", { name: "Disconnect this Mac" })).toBeTruthy();
    // 11h: the later-release rows are in the menu, dimmed and not buttons (11l).
    expect(menu.getByText("Coming in a later release")).toBeTruthy();
    for (const later of LATER) {
      const row = menu.getByText(later).closest("[data-not-built]");
      expect(row?.getAttribute("aria-disabled")).toBe("true");
      expect(menu.queryByRole("button", { name: later })).toBeNull();
    }

    await user.keyboard("{Escape}");
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("draws the server icon of 11h and 11k on the server menu's tile and on Settings' server tile (NG-22)", async () => {
    const { user } = await open(new FakeTransport());
    await user.click(titlebar().getByRole("button", { name: new RegExp(ADDRESS.replaceAll(".", "\\.")) }));
    expect(screen.getByRole("dialog", { name: "Server menu" }).querySelector(".menu-tile [data-icon=server]")).not.toBeNull();
    await user.keyboard("{Escape}");
    await user.click(rail().getByRole("button", { name: "Settings" }));
    expect(document.querySelector(".st-tile-mint [data-icon=server]")).not.toBeNull();
    expect(document.querySelector("[data-icon=activity]")).toBeNull();
  });

  it("shows the server on the Settings page: its state row, its Address row and Disconnect this Mac (11k)", async () => {
    const { user } = await open(new FakeTransport());
    await user.click(rail().getByRole("button", { name: "Settings" }));

    const page = within(screen.getByRole("main"));
    expect(page.getAllByText(ADDRESS).length).toBeGreaterThan(0);
    expect(await page.findByText(/^Connected · \d+ ms$/)).toBeTruthy();
    expect(page.getByText("Address")).toBeTruthy();
    expect(page.getByRole("button", { name: "Disconnect this Mac" })).toBeTruthy();
    for (const later of LATER) expect(screen.queryByText(later)).toBeNull();
  });

  it("disconnects: calls forget(), returns to 3a and names the revoke command with the device ID", async () => {
    const fake = new FakeTransport({ server: { address: ADDRESS, device: device("D-7", "Studio Mac") } });
    const { user } = await open(fake);
    await user.click(rail().getByRole("button", { name: "Settings" }));
    await user.click(screen.getByRole("button", { name: "Disconnect this Mac" }));
    // 11k: the confirm is in place; nothing is forgotten before Disconnect.
    expect(fake.count("forget")).toBe(0);
    await user.click(within(screen.getByRole("alertdialog")).getByRole("button", { name: "Disconnect" }));

    expect(await screen.findByText("Connect to your server")).toBeTruthy();
    expect(fake.count("forget")).toBe(1);
    expect(screen.getByText("pulse-factory device revoke D-7")).toBeTruthy();
    expect(screen.queryByRole("navigation")).toBeNull();
  });
});

// ---- the title bar on every page (r1.s6.w1, Approach 3; NG-2, NG-3; P3) ----

const beta = () =>
  chatSession("S-2", { project_id: "P-2", project_name: "beta", worktree_id: "W-2", worktree_dir: "/home/dev/beta" });

/** A booted shell with the chat S-1 open as a tab. */
async function withChat() {
  const fake = seeded({ sessions: [alpha(), beta()] });
  const user = await bootChat(fake, "S-1");
  return { fake, user };
}

const closeButton = (name: string) => titlebar().queryByRole("button", { name: `Close ${name}` });

describe("the title bar on the focused pages (11d, 11k)", () => {
  it.each(["Needs you", "Settings"])(
    "%s keeps the Projects tab, the chat tabs, New tab and the pill with its toggle, and hides only the sidebar toggle",
    async (page) => {
      const { user } = await withChat();
      expect(titlebar().getByRole("button", { name: "Show or hide the sidebar" })).toBeTruthy();
      await user.click(rail().getByRole("button", { name: page }));

      expect(titlebar().getByRole("button", { name: "Projects" })).toBeTruthy();
      expect(titlebar().getByRole("tab", { name: /Orchestrator · alpha/ })).toBeTruthy();
      expect(titlebar().getByRole("heading", { name: page })).toBeTruthy();
      expect(titlebar().getByRole("button", { name: "New tab" })).toBeTruthy();
      expect(titlebar().getByTitle("Show or hide Needs you")).toBeTruthy();
      expect(titlebar().queryByRole("button", { name: "Show or hide the sidebar" })).toBeNull();
      expect(titlebar().getByRole("button", { name: "Back" })).toBeTruthy();
      // NG-3: the active page tab carries its ×.
      expect(closeButton(page)).not.toBeNull();
    },
  );

  it("the pill's toggle still opens and closes the dock state from a focused page", async () => {
    const { user } = await withChat();
    await user.click(rail().getByRole("button", { name: "Settings" }));
    await user.click(titlebar().getByTitle("Show or hide Needs you"));
    expect(window.localStorage.getItem("pf.dock")).toBe("open");
  });
});

describe("the active page tab's × (NG-3; P3)", () => {
  it("Agents has its ×, and closes to the last chat tab used", async () => {
    const { user } = await withChat();
    await user.click(rail().getByRole("button", { name: "Agents" }));
    // r1.s6.w3 (W1-29): the Agents tab reads "<selected agent> agent".
    expect(titlebar().getByRole("heading", { name: "Orchestrator agent" })).toBeTruthy();
    await user.click(closeButton("Orchestrator agent")!);
    expect(titlebar().getByRole("tab", { name: /Orchestrator · alpha/ }).getAttribute("aria-selected")).toBe("true");
    expect(titlebar().queryByRole("heading", { name: "Orchestrator agent" })).toBeNull();
  });

  it.each(["Needs you", "Settings"])("%s closes to the last chat tab the operator used", async (page) => {
    const { user, fake } = await withChat();
    // A second chat, used after the first: it is the last one.
    await user.click(await within(screen.getByRole("region", { name: "Sidebar" })).findByRole("button", { name: /session in beta:/ }));
    await waitFor(() => expect(fake.current()?.options.session).toBe("S-2"));
    expect(titlebar().getByRole("tab", { name: /Orchestrator · beta/ }).getAttribute("aria-selected")).toBe("true");

    await user.click(rail().getByRole("button", { name: page }));
    await user.click(closeButton(page)!);
    expect(titlebar().getByRole("tab", { name: /Orchestrator · beta/ }).getAttribute("aria-selected")).toBe("true");
    expect(titlebar().getByRole("tab", { name: /Orchestrator · alpha/ }).getAttribute("aria-selected")).toBe("false");
  });

  it.each(["Needs you", "Settings", "Agents"])("%s closes to Projects when no chat tab is open", async (page) => {
    const { user } = await open(new FakeTransport());
    await user.click(rail().getByRole("button", { name: page }));
    await user.click(closeButton(page)!);
    expect(titlebar().getByRole("heading", { name: "Projects" })).toBeTruthy();
    expect(titlebar().queryByRole("heading", { name: page })).toBeNull();
  });

  it("closes to Projects when the last chat tab used has been closed", async () => {
    const { user } = await withChat();
    await user.click(titlebar().getByRole("button", { name: /^Close Orchestrator · alpha/ }));
    await user.click(rail().getByRole("button", { name: "Settings" }));
    await user.click(closeButton("Settings")!);
    expect(titlebar().getByRole("heading", { name: "Projects" })).toBeTruthy();
  });

  it("Projects has no × while no chat tab is open", async () => {
    await open(new FakeTransport());
    expect(titlebar().getByRole("heading", { name: "Projects" })).toBeTruthy();
    expect(closeButton("Projects")).toBeNull();
  });

  it("Projects with a chat tab open shows its ×, and the × shows the last chat tab", async () => {
    const { user } = await withChat();
    // The chat is on screen: the Projects tab is not the active one, so it has no ×.
    expect(closeButton("Projects")).toBeNull();
    await user.click(titlebar().getByRole("button", { name: "Projects" }));
    expect(titlebar().getByRole("heading", { name: "Projects" })).toBeTruthy();
    await user.click(closeButton("Projects")!);
    expect(titlebar().getByRole("tab", { name: /Orchestrator · alpha/ }).getAttribute("aria-selected")).toBe("true");
  });
});
