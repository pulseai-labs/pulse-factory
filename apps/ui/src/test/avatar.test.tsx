// AC-3 of `r1.s5.w1`: each 9a head's body path and eye equal the canvas's values, the four built-in
// agents use their colour and head, and the avatar's CSS holds no `@keyframes` and no `animation`.
//
// The expected values are copied from docs/specs/r1/r1.s5-…/canvas/agent-avatars.dc.html (the AI
// workspace snapshot of the canvas's approved 9a): `rr` is line 153, `BOTS` lines 155–164, `BOT_EYE`
// line 176, the `boteye` branch of `avatar()` lines 198–203 (the geometry of the eye) and the
// picks of 9a line 222 (the four built-in agents), with `PALETTE` at line 109. They are copied here as
// the source writes them, so the component's literal paths are checked against the canvas's own
// generator and not against themselves.
import { render } from "@testing-library/react";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { Avatar, BUILT_IN_AGENTS, HEADS, type HeadId } from "../ui";

// agent-avatars.dc.html:153
const rr = (x: number, y: number, w: number, h: number, r: number) =>
  "M" + (x + r) + " " + y + "h" + (w - 2 * r) + "a" + r + " " + r + " 0 0 1 " + r + " " + r + "v" + (h - 2 * r) + "a" + r + " " + r + " 0 0 1 " + -r + " " + r + "h" + -(w - 2 * r) + "a" + r + " " + r + " 0 0 1 " + -r + " " + -r + "v" + -(h - 2 * r) + "a" + r + " " + r + " 0 0 1 " + r + " " + -r + "Z";

// agent-avatars.dc.html:155-164 (`v`, `light` and the visor are the 8a variant and are not drawn by 9a)
const BOTS: Array<{ id: HeadId; body: string; ant?: true; ears?: string }> = [
  { id: "block", body: rr(5, 5, 30, 30, 8) },
  { id: "dome", body: "M5 35V20a15 15 0 0 1 30 0v15Z" },
  { id: "capsule", body: rr(8, 3, 24, 34, 12) },
  { id: "octa", body: "M14 4h12l10 10v12L26 36H14L4 26V14Z" },
  { id: "wide", body: rr(2, 9, 36, 25, 8) },
  { id: "antenna", body: rr(5, 11, 30, 25, 7), ant: true },
  { id: "ears", body: rr(7, 6, 26, 28, 8), ears: rr(2, 15, 6, 10, 2) + rr(32, 15, 6, 10, 2) },
  { id: "tall", body: rr(9, 3, 22, 34, 7) },
];

// agent-avatars.dc.html:176
const BOT_EYE: Record<HeadId, [number, number, number]> = {
  block: [20, 18, 7.5], dome: [20, 22, 7.5], capsule: [20, 17, 7.5], octa: [20, 19, 8],
  wide: [20, 21, 7], antenna: [20, 23, 7.5], ears: [20, 19, 7.5], tall: [20, 15, 7],
};

// agent-avatars.dc.html:222, the picks of 9a, with the colours of PALETTE (line 109): violet, red, mint, gray.
const PICKS: Array<[agent: "orchestrator" | "autopilot" | "research_partner" | "plain", colour: string, head: HeadId]> = [
  ["orchestrator", "var(--avatar-violet)", "antenna"],
  ["autopilot", "var(--avatar-red)", "dome"],
  ["research_partner", "var(--avatar-mint)", "capsule"],
  ["plain", "var(--avatar-gray)", "block"],
];

describe("the 9a heads", () => {
  it("has the eight heads of the canvas and no other", () => {
    expect(Object.keys(HEADS).sort()).toEqual(BOTS.map((b) => b.id).sort());
  });

  it.each(BOTS)("$id: the body path equals the canvas's", (bot) => {
    expect(HEADS[bot.id].body).toBe(bot.body);
    expect(HEADS[bot.id].ears).toBe(bot.ears);
    expect(HEADS[bot.id].antenna).toBe(bot.ant);
  });

  it.each(BOTS)("$id: the eye equals the canvas's", (bot) => {
    expect([...HEADS[bot.id].eye]).toEqual(BOT_EYE[bot.id]);
  });

  it.each(BOTS)("$id: the rendered head draws that body, socket and eye", (bot) => {
    const { container } = render(<Avatar agent={{ color: "var(--avatar-gray)", head: bot.id }} size={40} />);
    const svg = container.querySelector("svg") as SVGElement;
    expect(svg.getAttribute("viewBox")).toBe("0 0 40 40");
    expect(svg.getAttribute("aria-hidden")).toBe("true");
    const fills = [...svg.querySelectorAll("path.av-fill")].map((p) => p.getAttribute("d"));
    expect(fills).toContain(bot.body);
    if (bot.ears) expect(fills).toContain(bot.ears);
    const [ex, ey, er] = BOT_EYE[bot.id];
    const num = (selector: string, attr: string) => Number(svg.querySelector(selector)?.getAttribute(attr));
    // The socket (agent-avatars.dc.html:199-201: sr = er + 2.2, ir = er * 0.56, pr = er * 0.27,
    // the glint at ex + er * 0.2, ey - er * 0.22 with radius er * 0.11).
    expect(num(".av-socket", "cx")).toBe(ex);
    expect(num(".av-socket", "cy")).toBe(ey);
    expect(num(".av-socket", "r")).toBeCloseTo(er + 2.2, 6);
    expect(num(".av-sclera", "r")).toBe(er);
    expect(num(".av-iris", "r")).toBeCloseTo(er * 0.56, 6);
    expect(num(".av-pupil", "r")).toBeCloseTo(er * 0.27, 6);
    expect(num(".av-glint", "cx")).toBeCloseTo(ex + er * 0.2, 6);
    expect(num(".av-glint", "cy")).toBeCloseTo(ey - er * 0.22, 6);
    expect(num(".av-glint", "r")).toBeCloseTo(er * 0.11, 6);
    // The antenna head has its stalk and ball; no other head has them.
    expect(svg.querySelector(".av-stalk") !== null).toBe(bot.ant === true);
    if (bot.ant) {
      expect(svg.querySelector(".av-stalk")?.getAttribute("d")).toBe("M20 11V6");
      expect(num(".av-ball", "cy")).toBe(4.5);
      expect(num(".av-ball", "r")).toBe(2.4);
    }
  });
});

describe("the four built-in agents", () => {
  it.each(PICKS)("%s uses its colour and its head", (agent, colour, head) => {
    expect(BUILT_IN_AGENTS[agent]).toEqual({ color: colour, head });
    const { container } = render(<Avatar agent={agent} size={28} />);
    const root = container.firstElementChild as HTMLElement;
    expect(root.getAttribute("data-head")).toBe(head);
    expect(root.getAttribute("data-agent")).toBe(agent);
    expect(root.style.getPropertyValue("--avatar-c")).toBe(colour);
  });

  it("has those four and no more", () => {
    expect(Object.keys(BUILT_IN_AGENTS).sort()).toEqual(PICKS.map((p) => p[0]).sort());
  });
});

describe("the avatar's sizes, states and custom form", () => {
  it.each([16, 20, 28, 40] as const)("is %i px square", (size) => {
    const { container } = render(<Avatar agent="plain" size={size} />);
    const root = container.firstElementChild as HTMLElement;
    expect(root.style.width).toBe(`${size}px`);
    expect(root.style.height).toBe(`${size}px`);
    expect(root.className.includes("ui-avatar-sm")).toBe(size <= 18);
  });

  it("needs_you adds the amber badge and idle does not", () => {
    const idle = render(<Avatar agent="orchestrator" size={28} />);
    expect(idle.container.querySelector(".av-badge")).toBeNull();
    idle.unmount();
    const waiting = render(<Avatar agent="orchestrator" size={28} state="needs_you" />);
    const badge = waiting.container.querySelector(".av-badge") as SVGElement;
    expect(badge.getAttribute("cx")).toBe("33.5");
    expect(badge.getAttribute("cy")).toBe("6.5");
    expect(badge.getAttribute("r")).toBe("5");
  });

  it("takes a colour and a head for a later custom agent", () => {
    const { container } = render(<Avatar agent={{ color: "var(--avatar-red)", head: "octa" }} size={20} />);
    const root = container.firstElementChild as HTMLElement;
    expect(root.getAttribute("data-head")).toBe("octa");
    expect(root.getAttribute("data-agent")).toBeNull();
    expect(root.style.getPropertyValue("--avatar-c")).toBe("var(--avatar-red)");
  });
});

describe("the avatar's CSS", () => {
  const css = readFileSync(join(__dirname, "..", "ui", "ui.css"), "utf8");
  const start = css.indexOf("/* avatar:start */");
  const end = css.indexOf("/* avatar:end */");

  it("has its own marked block", () => {
    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    expect(css.slice(start, end)).toContain(".ui-avatar");
  });

  it("holds no @keyframes and no animation, in the block or in the file", () => {
    const block = css.slice(start, end);
    for (const word of ["@keyframes", "animation"]) {
      expect(block.includes(word), `the avatar block has ${word}`).toBe(false);
      expect(css.includes(word), `ui.css has ${word}`).toBe(false);
    }
  });

  it("the component sets no animation either", () => {
    const tsx = readFileSync(join(__dirname, "..", "ui", "Avatar.tsx"), "utf8");
    expect(/<animate|animation|@keyframes/i.test(tsx.replace(/\/\/.*$/gm, ""))).toBe(false);
  });
});
