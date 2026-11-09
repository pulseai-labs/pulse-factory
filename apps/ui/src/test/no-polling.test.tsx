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
    expect(fake.calls).toEqual(["paired", "projectList", "openStream"]);
    expect(screen.getByRole("region", { name: "Sidebar" })).toBeTruthy();

    await wiggle();
    expect(fake.calls).toEqual(["paired", "projectList", "openStream"]);
  });

  it("makes no call on the Settings page for an hour, blurred or focused", async () => {
    const fake = new FakeTransport();
    render(<App transport={fake} />);
    await advance(0);
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    await advance(0);
    expect(fake.calls).toEqual(["paired", "projectList", "openStream", "status"]);

    await wiggle();
    expect(fake.calls).toEqual(["paired", "projectList", "openStream", "status"]);
  });

  it("makes no call on the pair screen for an hour", async () => {
    const fake = new FakeTransport({ server: null });
    render(<App transport={fake} />);
    await advance(0);
    await wiggle();
    expect(fake.calls).toEqual(["paired"]);
  });
});
