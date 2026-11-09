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
  it("has a rail with exactly Projects, Agents and Settings, and nothing the later spines add", async () => {
    await open(new FakeTransport());
    expect(rail().getAllByRole("button").map((b) => b.getAttribute("aria-label"))).toEqual([
      "Projects",
      "Agents",
      "Settings",
    ]);
    for (const text of [/needs you/i, /need you/i, /more/i, /usage/i]) {
      expect(screen.queryByText(text)).toBeNull();
    }
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

  it("keeps only the sidebar's open state in localStorage, and reads it back", async () => {
    const fake = new FakeTransport();
    const { user, view } = await open(fake);
    expect(Object.keys(window.localStorage)).toEqual(["pf.sidebar"]);
    expect(window.localStorage.getItem("pf.sidebar")).toBe("open");

    await user.keyboard("{Meta>}b{/Meta}");
    expect(Object.keys(window.localStorage)).toEqual(["pf.sidebar"]);
    expect(window.localStorage.getItem("pf.sidebar")).toBe("closed");

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

  it("opens the server menu from the server's name and shows what this spine builds", async () => {
    const { user } = await open(new FakeTransport());
    expect(screen.queryByRole("dialog")).toBeNull();
    await user.click(within(sidebar() as HTMLElement).getByRole("button", { name: new RegExp(ADDRESS.replaceAll(".", "\\.")) }));

    const menu = within(screen.getByRole("dialog", { name: "Server menu" }));
    expect(menu.getAllByText(ADDRESS).length).toBeGreaterThan(0);
    expect(menu.getByText(SERVER_META)).toBeTruthy();
    expect(menu.getByRole("button", { name: "Disconnect this Mac" })).toBeTruthy();
    for (const later of LATER) expect(menu.queryByText(later)).toBeNull();

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
