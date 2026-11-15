// `r1.s6.w1`, Approach 1: the app names the server by the name the server's `status()` answer gave, at the nine
// sites that read it, and shows the address only when no name is known (P1). The pairing the UI reads at
// start has no `server` (`pf-client` fills it in memory from an answer), so the name must come from the mount
// call of the connection.
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "../App";
import { serverNameOf } from "../state/connection";
import { alpha, boot, push, seeded } from "./chat-helpers";
import { ADDRESS, FakeTransport, SERVER, device, folderEntry, folderList, frames, makeCode, project } from "./fake-transport";

const NAME = "draco-desk";
const KEY = "pf.server-name";
const NAMED = { ...SERVER, name: NAME };

afterEach(() => {
  window.localStorage.clear();
  vi.restoreAllMocks();
});

const titlebar = () => within(screen.getByRole("banner"));
const main = () => within(screen.getByRole("main"));

/** A fake that names its server only in the `status()` answer, with one project, one session and a home folder. */
function named(): FakeTransport {
  const fake = seeded({ projects: [project(1, "alpha"), project(2, "beta")], sessions: [alpha()] });
  fake.statusServer = NAMED;
  fake.folders["/home/dev"] = folderList([folderEntry("scratch")]);
  return fake;
}

async function open(fake: FakeTransport) {
  const user = userEvent.setup();
  render(<App transport={fake} />);
  await screen.findByRole("navigation");
  await waitFor(() => expect(fake.count("projectList")).toBe(1));
  return user;
}

describe("serverNameOf (P1)", () => {
  const paired = { address: ADDRESS, device: device() };

  it("takes the learned name, else the pairing's, else the stored name for the same address, else the address", () => {
    expect(serverNameOf(paired)).toBe(ADDRESS);
    expect(serverNameOf({ ...paired, server: NAMED })).toBe(NAME);
    expect(serverNameOf({ ...paired, server: NAMED }, "learned")).toBe("learned");
    window.localStorage.setItem(KEY, JSON.stringify({ address: ADDRESS, name: "stored" }));
    expect(serverNameOf(paired)).toBe("stored");
    expect(serverNameOf(paired, "learned")).toBe("learned");
    expect(serverNameOf({ ...paired, server: NAMED })).toBe(NAME);
  });

  it("ignores a stored name for another address and a stored value that is not {address, name}", () => {
    window.localStorage.setItem(KEY, JSON.stringify({ address: "10.0.0.9:7443", name: "other" }));
    expect(serverNameOf(paired)).toBe(ADDRESS);
    for (const bad of ["not json", "null", "[]", JSON.stringify({ address: ADDRESS }), JSON.stringify({ address: ADDRESS, name: 4 })]) {
      window.localStorage.setItem(KEY, bad);
      expect(serverNameOf(paired)).toBe(ADDRESS);
    }
  });

  it("falls back to the address, with no error, when localStorage throws", () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("storage is blocked");
    });
    expect(serverNameOf(paired)).toBe(ADDRESS);
  });
});

describe("the nine sites", () => {
  it("one status() call at mount names the server in the title bar, with no timer", async () => {
    const fake = named();
    await open(fake);
    await waitFor(() => expect(titlebar().getByRole("button", { name: NAME })).toBeTruthy());
    expect(fake.count("status")).toBe(1);
    expect(titlebar().queryByText(ADDRESS)).toBeNull();
    expect(titlebar().getByRole("button", { name: NAME }).getAttribute("title")).not.toContain(ADDRESS);
  });

  it("the server menu names the server and keeps the address in its Address row only", async () => {
    const fake = named();
    const user = await open(fake);
    await user.click(await titlebar().findByRole("button", { name: NAME }));
    const menu = within(screen.getByRole("dialog", { name: "Server menu" }));
    expect(menu.getByText(NAME)).toBeTruthy();
    expect(menu.getAllByText(ADDRESS)).toHaveLength(1);
    await user.click(menu.getByRole("button", { name: "Disconnect this Mac" }));
    expect(screen.getByRole("alertdialog", { name: `Disconnect this Mac from ${NAME}?` })).toBeTruthy();
  });

  it("the banner and the Projects subtitle name the server while the stream reconnects", async () => {
    const fake = named();
    await boot(fake);
    await screen.findByRole("button", { name: NAME });
    await act(async () => {
      fake.emit({ event: "reconnecting", after: 0, retry_in_ms: 4000, reason: "the stream dropped" });
    });
    const banner = (await screen.findByText(`Not connected to ${NAME}`)).closest(".banner") as HTMLElement;
    expect(banner.textContent).not.toContain(ADDRESS);
    expect(main().getByText(new RegExp(`^As of \\d\\d:\\d\\d, when ${NAME} was last reachable$`))).toBeTruthy();
    expect(main().queryByText(new RegExp(ADDRESS.replaceAll(".", "\\.")))).toBeNull();
  });

  it("the Add a project dialog and the remove confirm hold the name and no address", async () => {
    const fake = named();
    const user = await boot(fake);
    await push(fake, frames.hello(fake.sessions));
    await screen.findByRole("button", { name: NAME });

    await user.click(await main().findByRole("button", { name: "Remove beta" }));
    const confirm = within(await screen.findByRole("alertdialog", { name: "Remove beta" }));
    expect(confirm.getByText(new RegExp(`stays on ${NAME}\\.`))).toBeTruthy();
    expect(confirm.queryByText(new RegExp(ADDRESS.replaceAll(".", "\\.")))).toBeNull();
    await user.click(confirm.getByRole("button", { name: "Cancel" }));

    await user.click(main().getByRole("button", { name: "Add a project" }));
    const dialog = within(await screen.findByRole("dialog", { name: "Add a project" }));
    await dialog.findByText("scratch");
    expect(dialog.getByText(NAME)).toBeTruthy();
    expect(dialog.queryByText(new RegExp(ADDRESS.replaceAll(".", "\\.")))).toBeNull();
  });

  it("the End session strip names the server", async () => {
    const fake = named();
    const user = await boot(fake);
    await push(fake, frames.hello(fake.sessions));
    await screen.findByRole("button", { name: NAME });
    await user.click(await within(screen.getByRole("region", { name: "Sidebar" })).findByRole("button", { name: /session in alpha:/ }));
    await user.click(await main().findByRole("button", { name: "End session" }));
    expect(main().getByText(new RegExp(`process on ${NAME} stops`))).toBeTruthy();
  });

  it("Settings names the server from its own status() call", async () => {
    const fake = new FakeTransport();
    fake.statusServer = NAMED;
    const user = await open(fake);
    await user.click(within(screen.getByRole("navigation")).getByRole("button", { name: "Settings" }));
    await waitFor(() => expect(document.querySelector(".st-server-name")?.textContent).toBe(NAME));
  });

  it("the pair card names the server after a pairing, and the unpaired chip still reads No server (11j)", async () => {
    const fake = new FakeTransport({ server: null });
    fake.statusServer = NAMED;
    const user = userEvent.setup();
    render(<App transport={fake} />);
    expect(await screen.findByText("No server")).toBeTruthy();
    await user.click(screen.getByLabelText("Pairing code"));
    await user.paste(makeCode(ADDRESS));
    await user.click(screen.getByRole("button", { name: "Pair" }));
    expect(await screen.findByRole("heading", { name: `Connected to ${NAME}` })).toBeTruthy();
    expect(within(screen.getByRole("banner")).getByText(NAME)).toBeTruthy();
    expect(fake.count("status")).toBe(0);
  });
});

describe("the stored name (P1)", () => {
  it("shows at once, with the server unreachable from launch, when it is for the same address", async () => {
    window.localStorage.setItem(KEY, JSON.stringify({ address: ADDRESS, name: NAME }));
    const fake = new FakeTransport();
    fake.fail("status", "unreachable", "no answer");
    fake.fail("projectList", "unreachable", "no answer");
    await open(fake);
    expect(titlebar().getByRole("button", { name: NAME })).toBeTruthy();
    expect(await screen.findByText(`Not connected to ${NAME}`)).toBeTruthy();
  });

  it("is ignored when it is for another address", async () => {
    window.localStorage.setItem(KEY, JSON.stringify({ address: "10.0.0.9:7443", name: "other-box" }));
    const fake = new FakeTransport();
    fake.fail("status", "unreachable", "no answer");
    fake.fail("projectList", "unreachable", "no answer");
    await open(fake);
    expect(titlebar().getByRole("button", { name: ADDRESS })).toBeTruthy();
    expect(screen.queryByText(/other-box/)).toBeNull();
  });

  it("keeps the last name an answer gave, for this address", async () => {
    const fake = named();
    await open(fake);
    await screen.findByRole("button", { name: NAME });
    expect(JSON.parse(window.localStorage.getItem(KEY) ?? "null")).toEqual({ address: ADDRESS, name: NAME });
  });

  it("falls back to the address, with no error, when localStorage fails", async () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation((key: string) => {
      if (key === KEY) throw new Error("storage is blocked");
      return null;
    });
    vi.spyOn(Storage.prototype, "setItem").mockImplementation((key: string) => {
      if (key === KEY) throw new Error("storage is blocked");
    });
    const fake = new FakeTransport();
    fake.fail("status", "unreachable", "no answer");
    fake.fail("projectList", "unreachable", "no answer");
    await open(fake);
    expect(titlebar().getByRole("button", { name: ADDRESS })).toBeTruthy();
  });
});

describe("with localStorage blocked", () => {
  it("an answer that names the server still names it", async () => {
    vi.spyOn(Storage.prototype, "setItem").mockImplementation((key: string) => {
      if (key === KEY) throw new Error("storage is blocked");
    });
    await open(named());
    expect(await screen.findByRole("button", { name: NAME })).toBeTruthy();
  });
});
