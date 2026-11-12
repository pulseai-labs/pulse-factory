import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { App } from "../App";
import { ADDRESS, FakeTransport, device, project } from "./fake-transport";

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
const SERVER_META = new RegExp(`Connected · \\d+ ms · ${ADDRESS.replaceAll(".", "\\.")}`);
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
    expect(titlebar().queryByText("Projects")).toBeNull();
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

  it("shows the same server section on the Settings page", async () => {
    const { user } = await open(new FakeTransport());
    await user.click(rail().getByRole("button", { name: "Settings" }));

    const page = within(screen.getByRole("main"));
    expect(page.getAllByText(ADDRESS).length).toBeGreaterThan(0);
    expect(await page.findByText(SERVER_META)).toBeTruthy();
    expect(page.getByRole("button", { name: "Disconnect this Mac" })).toBeTruthy();
    for (const later of LATER) expect(screen.queryByText(later)).toBeNull();
  });

  it("disconnects: calls forget(), returns to 3a and names the revoke command with the device ID", async () => {
    const fake = new FakeTransport({ server: { address: ADDRESS, device: device("D-7", "Studio Mac") } });
    const { user } = await open(fake);
    await user.click(rail().getByRole("button", { name: "Settings" }));
    await user.click(screen.getByRole("button", { name: "Disconnect this Mac" }));

    expect(await screen.findByText("Connect to your server")).toBeTruthy();
    expect(fake.count("forget")).toBe(1);
    expect(screen.getByText("pulse-factory device revoke D-7")).toBeTruthy();
    expect(screen.queryByRole("navigation")).toBeNull();
  });
});
