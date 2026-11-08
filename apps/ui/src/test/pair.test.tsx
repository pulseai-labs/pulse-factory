import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { App } from "../App";
import { ADDRESS, FakeTransport, makeCode } from "./fake-transport";

function setup(fake: FakeTransport) {
  const user = userEvent.setup();
  render(<App transport={fake} />);
  return user;
}

async function fillAndPair(user: ReturnType<typeof userEvent.setup>, code: string, name?: string) {
  const codeInput = await screen.findByLabelText("Pairing code");
  await user.clear(codeInput);
  await user.click(codeInput);
  await user.paste(code);
  if (name !== undefined) {
    const nameInput = screen.getByLabelText("Device name");
    await user.clear(nameInput);
    if (name !== "") await user.type(nameInput, name);
  }
  await user.click(screen.getByRole("button", { name: "Pair" }));
}

describe("the pair screen (3a)", () => {
  it("opens on 3a when no server is paired, with the command and the default device name", async () => {
    const fake = new FakeTransport({ server: null });
    setup(fake);
    expect(await screen.findByText("Connect to your server")).toBeTruthy();
    expect(screen.getByText("pulse-factory device pair")).toBeTruthy();
    expect((screen.getByLabelText("Device name") as HTMLInputElement).value).toBe("MacBook");
    expect(fake.calls).toEqual(["paired"]);
  });

  it("refuses a code that does not start with pf1- before any call", async () => {
    const fake = new FakeTransport({ server: null });
    const user = setup(fake);
    await fillAndPair(user, "123456");
    expect((await screen.findByRole("alert")).textContent).toMatch(/pf1-/);
    expect(fake.count("pair")).toBe(0);
  });

  it.each([
    ["code_refused", "the code is not accepted", /code is not accepted/i, /new code/i],
    ["unreachable", "connection refused", new RegExp(ADDRESS.replaceAll(".", "\\.")), undefined],
    ["key_changed", "server key changed: re-pair", /^server key changed: re-pair$/, /new code/i],
    ["rate_limited", "too many failed attempts; try later", /too many failed attempts; try later/, undefined],
  ] as const)("shows the %s error and keeps the form", async (kind, message, expected, hint) => {
    const fake = new FakeTransport({ server: null });
    fake.fail("pair", kind, message);
    const user = setup(fake);
    const code = makeCode(ADDRESS);
    await fillAndPair(user, code, "Studio Mac");

    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText(expected)).toBeTruthy();
    if (hint) expect(alert.textContent).toMatch(hint);
    expect((screen.getByLabelText("Pairing code") as HTMLInputElement).value).toBe(code);
    expect((screen.getByLabelText("Device name") as HTMLInputElement).value).toBe("Studio Mac");
    expect((screen.getByRole("button", { name: "Pair" }) as HTMLButtonElement).disabled).toBe(false);
    expect(screen.queryByText("Open Pulse Factory")).toBeNull();
  });

  it("pairs with a good code, shows the address and the name, then opens the shell", async () => {
    const fake = new FakeTransport({ server: null });
    const user = setup(fake);
    await fillAndPair(user, `  ${makeCode(ADDRESS).toUpperCase()}\n`, "d12 browser");

    expect(await screen.findByText(`Connected to ${ADDRESS}`)).toBeTruthy();
    expect(screen.getByText("d12 browser")).toBeTruthy();
    // The code goes to the transport trimmed and in lower case, and the name as typed.
    expect(fake.pairArgs).toEqual([{ code: makeCode(ADDRESS), deviceName: "d12 browser" }]);
    expect(fake.count("projectList")).toBe(0);

    await user.click(screen.getByRole("button", { name: "Open Pulse Factory" }));
    expect(await screen.findByRole("navigation")).toBeTruthy();
    expect(within(screen.getByRole("banner")).getByText("Projects")).toBeTruthy();
    expect(fake.count("projectList")).toBe(1);
  });

  it("holds the device-name rule: 1 to 64 characters", async () => {
    const fake = new FakeTransport({ server: null });
    const user = setup(fake);
    const code = makeCode(ADDRESS);

    await fillAndPair(user, code, "");
    expect((await screen.findByRole("alert")).textContent).toMatch(/1 to 64 characters/);
    expect(fake.count("pair")).toBe(0);

    await fillAndPair(user, code, "x".repeat(65));
    expect((await screen.findByRole("alert")).textContent).toMatch(/1 to 64 characters/);
    expect(fake.count("pair")).toBe(0);

    await fillAndPair(user, code, "x".repeat(64));
    expect(await screen.findByText(`Connected to ${ADDRESS}`)).toBeTruthy();
    expect(fake.count("pair")).toBe(1);
  });
});
