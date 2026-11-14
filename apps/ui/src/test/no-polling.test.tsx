// C15: while connected, no timer, interval or focus handler makes a call.
import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../App";
import { FakeTransport, project } from "./fake-transport";

const THIRTY_MINUTES = 30 * 60 * 1000;

beforeEach(() => {
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
});

async function advance(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

async function wiggle() {
  // Thirty minutes of timers, with the window blurred and focused and the page hidden and shown.
  await advance(THIRTY_MINUTES);
  await act(async () => {
    window.dispatchEvent(new Event("blur"));
    document.dispatchEvent(new Event("visibilitychange"));
    window.dispatchEvent(new Event("focus"));
    window.dispatchEvent(new Event("online"));
  });
  await advance(THIRTY_MINUTES);
}

describe("the UI never polls", () => {
  it("makes no call on the Projects page for an hour, blurred or focused", async () => {
    const fake = new FakeTransport({ projects: [project(1, "alpha")] });
    render(<App transport={fake} />);
    await advance(0);
    // The shell's sidebar reads the run list once when it mounts (Approach 7).
    // The connection's one `status()` call at mount (r1.s6.w1) names the server; it is no timer.
    const opened = ["paired", "runList", "projectList", "openStream", "status"];
    expect(fake.calls).toEqual(opened);
    expect(screen.getByRole("region", { name: "Sidebar" })).toBeTruthy();

    await wiggle();
    expect(fake.calls).toEqual(opened);
  });

  it("makes no call on the Settings page for an hour, blurred or focused", async () => {
    const fake = new FakeTransport();
    render(<App transport={fake} />);
    await advance(0);
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    await advance(0);
    // r1.s4.w3: the page also reads its settings, and the session list for its Agent select (once, as
    // the list is not loaded yet).
    // The shell's sidebar read the run list once before the page opened.
    const opened = ["paired", "runList", "projectList", "openStream", "status", "settingsGet", "sessionList", "status"];
    expect(fake.calls).toEqual(opened);

    await wiggle();
    expect(fake.calls).toEqual(opened);
  });

  it("makes no call on the Needs you page for an hour, blurred or focused", async () => {
    const fake = new FakeTransport();
    render(<App transport={fake} />);
    await advance(0);
    fireEvent.click(screen.getByRole("button", { name: "Needs you" }));
    await advance(0);
    // r1.s4.w3: the requests and the runs, once, when the page opens; no stream of its own. The shell's
    // sidebar read the run list once before, on Projects.
    const opened = ["paired", "runList", "projectList", "openStream", "status", "requests", "runList"];
    expect(fake.calls).toEqual(opened);

    await wiggle();
    expect(fake.calls).toEqual(opened);
  });

  it("makes no call by a timer while the link is up and the stream is reconnecting (r1.s6.w1)", async () => {
    const fake = new FakeTransport({ projects: [project(1, "alpha")] });
    render(<App transport={fake} />);
    await advance(0);
    await act(async () => {
      fake.emit({ event: "reconnecting", after: 0, retry_in_ms: 4000, reason: "the stream dropped" });
    });
    // The banner counts down on a display tick, which makes no call, and the stream's own retry is `pf-client`'s.
    expect(screen.getByText(/Retrying in \ds\./)).toBeTruthy();
    const calls = [...fake.calls];
    await wiggle();
    expect(fake.calls).toEqual(calls);
    expect(screen.getByText(/Retrying in 0s\./)).toBeTruthy();
  });

  it("makes no call on the pair screen for an hour", async () => {
    const fake = new FakeTransport({ server: null });
    render(<App transport={fake} />);
    await advance(0);
    await wiggle();
    expect(fake.calls).toEqual(["paired"]);
  });
});
