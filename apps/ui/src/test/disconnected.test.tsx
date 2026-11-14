// Fake timers drive these tests. Testing Library's async helpers do not advance vitest's fake
// timers, so the tests flush by hand and read the screen with `getBy*`.
import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../App";
import { ADDRESS, FakeTransport, folderEntry, folderList, frames, project } from "./fake-transport";

const TEN_MINUTES = 10 * 60 * 1000;

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date(2026, 10, 7, 14, 38, 0));
});
afterEach(() => {
  vi.useRealTimers();
});

async function advance(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

async function boot(fake: FakeTransport) {
  render(<App transport={fake} />);
  await advance(0);
}

// Every call list starts "paired", "runList", ...: the shell's sidebar reads the run list once when it mounts.
const click = (name: string) => fireEvent.click(screen.getByRole("button", { name }));

/** Adds the folder `name` of the home folder through the Add a project dialog (`r1.s5.w5`; the page's own form is gone). */
async function addFolder(fake: FakeTransport, name: string) {
  fake.folders["/home/dev"] = folderList([folderEntry(name)]);
  click("Add a project");
  await advance(0);
  fireEvent.click(screen.getByRole("button", { name: new RegExp(`^${name}`) }));
  click("Add project");
  await advance(0);
}

describe("the disconnected screen (3c)", () => {
  it("unreachable: retries only status() at 4, 8, 16, 32 and 60 seconds, then recovers", async () => {
    const fake = new FakeTransport({ projects: [project(1, "alpha")] });
    fake.fail("projectList", "unreachable", "no answer");
    fake.fail("status", "unreachable", "no answer");
    await boot(fake);

    expect(screen.getByText(`Not connected to ${ADDRESS}`)).toBeTruthy();
    expect(screen.getByText(/Retrying in 4s/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Retry now" })).toBeTruthy();
    // The one `status()` call the connection makes when it mounts (r1.s6.w1) is the first of them.
    expect(fake.calls).toEqual(["paired", "runList", "projectList", "openStream", "status"]);

    await advance(1000);
    expect(screen.getByText(/Retrying in 3s/)).toBeTruthy();
    await advance(2999);
    expect(fake.count("status")).toBe(1);
    await advance(1); // 4 s
    expect(fake.count("status")).toBe(2);
    expect(screen.getByText(/Retrying in 8s/)).toBeTruthy();

    // 8, 16, 32, 60, 60 seconds after each failed retry.
    for (const [seconds, total] of [
      [8, 3],
      [16, 4],
      [32, 5],
      [60, 6],
      [60, 7],
    ] as const) {
      await advance(seconds * 1000 - 1);
      expect(fake.count("status")).toBe(total - 1);
      await advance(1);
      expect(fake.count("status")).toBe(total);
    }
    // Only `status()` was called by the timer. The page fetched once, on opening.
    expect(fake.calls).toEqual(["paired", "runList", "projectList", "openStream", ...Array(7).fill("status")]);

    // The server comes back: the next retry clears the banner and fetches the open page once.
    fake.heal();
    await advance(60_000);
    expect(screen.queryByText(/Not connected/)).toBeNull();
    // The page fetches once, the shell's sidebar reads the run list once, and the chat state reads the
    // session list once.
    expect(fake.calls).toEqual([
      "paired",
      "runList",
      "projectList",
      "openStream",
      ...Array(8).fill("status"),
      "runList",
      "projectList",
      "sessionList",
    ]);
    expect(within(screen.getByRole("main")).getByText("alpha")).toBeTruthy();

    // Connected again: no timer calls.
    await advance(TEN_MINUTES);
    expect(fake.calls).toHaveLength(15);
  });

  it("unreachable: 'Retry now' calls status() at once", async () => {
    const fake = new FakeTransport();
    fake.fail("projectList", "unreachable", "no answer");
    fake.fail("status", "unreachable", "no answer");
    await boot(fake);
    click("Retry now");
    await advance(0);
    expect(fake.count("status")).toBe(2); // the mount call and this retry
    expect(screen.getByText(/Retrying in 8s/)).toBeTruthy();
  });

  it("unreachable: the lists keep their last data and say 'As of <time>'", async () => {
    const fake = new FakeTransport({ projects: [project(1, "alpha")] });
    await boot(fake);
    expect(screen.queryByText(/As of/)).toBeNull();

    await advance(60_000); // 14:39
    // The dialog's first call fails: nothing reached the server at 14:39.
    fake.fail("folderList", "unreachable", "no answer");
    fake.fail("status", "unreachable", "no answer");
    click("Add a project");
    await advance(0);

    expect(screen.getByText(`Not connected to ${ADDRESS}`)).toBeTruthy();
    expect(screen.getByText("last reached 14:38")).toBeTruthy();
    expect(screen.getByText(`As of 14:38, when ${ADDRESS} was last reachable`)).toBeTruthy();
    expect(within(screen.getByRole("main")).getByText("alpha")).toBeTruthy();
    expect(within(screen.getByRole("region", { name: "Sidebar" })).getByText("alpha")).toBeTruthy();
  });

  it("unauthorized: says the device is no longer paired, sets no timer, and 'Pair again' opens 3a", async () => {
    const fake = new FakeTransport();
    fake.fail("projectList", "unauthorized", "not authorized");
    // A server that refuses this device refuses `status()` too: the mount call must not read as an answer.
    fake.fail("status", "unauthorized", "not authorized");
    await boot(fake);

    expect(screen.getByText(`This device is no longer paired with ${ADDRESS}.`)).toBeTruthy();
    expect(screen.queryByText(/Retrying/)).toBeNull();
    expect(screen.queryByRole("button", { name: "Retry now" })).toBeNull();
    await advance(TEN_MINUTES);
    expect(fake.calls).toEqual(["paired", "runList", "projectList", "openStream", "status"]);

    click("Pair again");
    await advance(0);
    expect(screen.getByText("Connect to your server")).toBeTruthy();
  });

  it("key_changed: shows exactly 'server key changed: re-pair', sets no timer, and 'Pair again' opens 3a", async () => {
    const fake = new FakeTransport();
    fake.fail("projectList", "key_changed", "server key changed: re-pair");
    fake.fail("status", "key_changed", "server key changed: re-pair");
    await boot(fake);

    expect(screen.getByText(/^server key changed: re-pair$/)).toBeTruthy();
    await advance(TEN_MINUTES);
    expect(fake.calls).toEqual(["paired", "runList", "projectList", "openStream", "status"]);

    click("Pair again");
    await advance(0);
    expect(screen.getByText("Connect to your server")).toBeTruthy();
  });

  it("rate_limited: shows the server's text, sets no timer, and 'Retry now' calls status()", async () => {
    const fake = new FakeTransport();
    fake.fail("projectList", "rate_limited", "too many failed calls; wait a minute");
    fake.fail("status", "rate_limited", "too many failed calls; wait a minute");
    await boot(fake);

    expect(screen.getByText("too many failed calls; wait a minute")).toBeTruthy();
    expect(screen.queryByText(/Retrying/)).toBeNull();
    await advance(TEN_MINUTES);
    expect(fake.calls).toEqual(["paired", "runList", "projectList", "openStream", "status"]);

    click("Retry now");
    await advance(0);
    expect(fake.calls).toEqual(["paired", "runList", "projectList", "openStream", "status", "status"]);
    // Still limited: the banner stays and no timer starts.
    expect(screen.getByText("too many failed calls; wait a minute")).toBeTruthy();
    await advance(TEN_MINUTES);
    expect(fake.count("status")).toBe(2);

    fake.heal();
    click("Retry now");
    await advance(0);
    expect(screen.queryByText("too many failed calls; wait a minute")).toBeNull();
    expect(fake.calls).toEqual([
      "paired",
      "runList",
      "projectList",
      "openStream",
      "status",
      "status",
      "status",
      "runList",
      "projectList",
      "sessionList",
    ]);
  });

  it("a stream `reconnecting` event while the link is up shows 11i in full, and the next frame clears it (r1.s6.w1)", async () => {
    const fake = new FakeTransport({ projects: [project(1, "alpha")] });
    fake.statusServer = { id: "0".repeat(64), name: "draco-desk" };
    await boot(fake);
    expect(screen.queryByText(/Not connected/)).toBeNull();
    expect(document.querySelector(".app-down")).toBeNull();
    const pill = () => within(screen.getByRole("banner"));
    expect(pill().getByTitle("Show or hide Needs you")).toBeTruthy();

    await act(async () => {
      fake.emit({ event: "reconnecting", after: 0, retry_in_ms: 4000, reason: "the stream dropped" });
    });
    // The banner: 11i's copy, the countdown from `retry_in_ms`, the time the server was last reached.
    expect(screen.getByText("Not connected to draco-desk")).toBeTruthy();
    expect(screen.getByText("Your sessions and runs keep going on the server. Retrying in 4s.")).toBeTruthy();
    expect(screen.getByText("last reached 14:38")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Retry now" })).toBeTruthy();
    await advance(1000);
    expect(screen.getByText(/Retrying in 3s\./)).toBeTruthy();
    // The page dims, the Projects subtitle says when the server was last reachable, and the pill greys with
    // the time and no toggle.
    expect(document.querySelector(".app-down")).not.toBeNull();
    expect(screen.getByText("As of 14:38, when draco-desk was last reachable")).toBeTruthy();
    expect(pill().queryByTitle("Show or hide Needs you")).toBeNull();
    expect(pill().getByText("0 need you · 14:38")).toBeTruthy();
    // Nothing says "Reconnecting…" anywhere, and the display tick made no call.
    expect(screen.queryByText(/Reconnecting…/)).toBeNull();
    expect(fake.count("status")).toBe(1);

    await act(async () => {
      fake.frame(frames.hello([]));
    });
    expect(screen.queryByText(/Not connected/)).toBeNull();
    expect(document.querySelector(".app-down")).toBeNull();
    expect(screen.queryByText(/As of/)).toBeNull();
    expect(pill().getByTitle("Show or hide Needs you")).toBeTruthy();
  });

  it("while the link is down the banner counts the link's retry; Retry now retries the link, not the stream", async () => {
    const fake = new FakeTransport();
    fake.fail("projectList", "unreachable", "no answer");
    fake.fail("status", "unreachable", "no answer");
    await boot(fake);
    await act(async () => {
      fake.emit({ event: "reconnecting", after: 0, retry_in_ms: 30_000, reason: "the stream dropped" });
    });
    expect(screen.getByText(/Retrying in 4s\./)).toBeTruthy();
    const streams = fake.streams.length;
    click("Retry now");
    await advance(0);
    expect(fake.count("status")).toBe(2);
    expect(fake.streams.length).toBe(streams);
  });

  it.each([
    ["forbidden", "the verb \"project_list\" is not open to devices"],
    ["bad_request", "the body is not JSON"],
    ["server", "the controller failed inside the call"],
  ] as const)("%s from projectList(): one error line, the last data stays, no timer", async (kind, text) => {
    const fake = new FakeTransport({ projects: [project(1, "alpha")] });
    await boot(fake);
    expect(within(screen.getByRole("main")).getByText("alpha")).toBeTruthy();

    // A later fetch fails: an add succeeds, and the list fetch after it is refused.
    fake.fail("projectList", kind, text);
    await addFolder(fake, "beta");

    const alerts = screen.getAllByRole("alert");
    expect(alerts).toHaveLength(1);
    expect(within(alerts[0] as HTMLElement).getByText(text)).toBeTruthy();
    expect(within(alerts[0] as HTMLElement).getByRole("button", { name: "Retry now" })).toBeTruthy();
    // No disconnect: no banner, and the page keeps its last list.
    expect(screen.queryByText(/Not connected/)).toBeNull();
    expect(within(screen.getByRole("main")).getByText("alpha")).toBeTruthy();

    const before = fake.calls.length;
    await advance(TEN_MINUTES);
    expect(fake.calls).toHaveLength(before);

    fake.heal();
    click("Retry now");
    await advance(0);
    expect(fake.calls.length).toBe(before + 1);
    expect(screen.queryByRole("alert")).toBeNull();
    expect(within(screen.getByRole("main")).getByText("beta")).toBeTruthy();
  });

  it("forbidden from status() on the Settings page is one error line, no disconnect", async () => {
    const fake = new FakeTransport();
    await boot(fake);
    fake.fail("status", "forbidden", "status is not open to this device");
    click("Settings");
    await advance(0);

    expect(screen.getByText("status is not open to this device")).toBeTruthy();
    expect(screen.queryByText(/Not connected/)).toBeNull();
    const before = fake.calls.length;
    await advance(TEN_MINUTES);
    expect(fake.calls).toHaveLength(before);
  });
});
