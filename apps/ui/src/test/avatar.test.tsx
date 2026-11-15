// AC-3 of `r1.s5.w1`: each 9a head's body path and eye equal the canvas's values, and the four built-in
// agents use their colour and head. The patch `fix/ui-canvas-motion` (2026-11-15) replaced w1's "no animation"
// checks with the 9a motion: the avatar's CSS holds the canvas's keyframes and state rules, none at 16 px and
// none under reduced motion.
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
  it.each([16, 20, 22, 24, 26, 28, 40, 64] as const)("is %i px square", (size) => {
    const { container } = render(<Avatar agent="plain" size={size} />);
    const root = container.firstElementChild as HTMLElement;
    expect(root.style.width).toBe(`${size}px`);
    expect(root.style.height).toBe(`${size}px`);
    expect(root.className.includes("ui-avatar-sm")).toBe(size <= 18);
  });

  it.each([22, 24, 26, 64] as const)("at %i px the 40×40 view box scales and the geometry stays (Approach 11)", (size) => {
    const at40 = render(<Avatar agent="orchestrator" size={40} />);
    const reference = at40.container.querySelector("svg")?.innerHTML;
    at40.unmount();
    const { container } = render(<Avatar agent="orchestrator" size={size} />);
    const svg = container.querySelector("svg") as SVGElement;
    expect(svg.getAttribute("viewBox")).toBe("0 0 40 40");
    expect(svg.getAttribute("width")).toBe("100%");
    expect(svg.innerHTML).toBe(reference);
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
  /** CSS with no comment, no white space and no `;` before a `}`, and every bare `.5` written `0.5`. */
  const fold = (text: string) =>
    text
      .replace(/\/\*[\s\S]*?\*\//g, "")
      .replace(/\s+/g, "")
      .replace(/;\}/g, "}")
      .replace(/(^|[^\d])\.(\d)/g, "$10.$2");
  const block = () => fold(css.slice(start, end));

  it("has its own marked block", () => {
    expect(start).toBeGreaterThanOrEqual(0);
    expect(end).toBeGreaterThan(start);
    expect(css.slice(start, end)).toContain(".ui-avatar");
  });

  // r1-screens.dc.html, the style block at the top of the file; agent-avatars.dc.html lines 18-24 write the same
  // four keyframes for 9a. They are copied here as the canvas writes them.
  const KEYFRAMES = [
    "@keyframes lvBlink{0%,93%,100%{transform:scaleY(1)}96%{transform:scaleY(.1)}}",
    "@keyframes lvPing{0%,100%{opacity:1}50%{opacity:.3}}",
    "@keyframes lvLook{0%,16%{transform:translate(0,0)}24%,44%{transform:translate(-2.6px,-.8px)}52%,70%{transform:translate(2.6px,.6px)}78%,100%{transform:translate(0,0)}}",
    "@keyframes lvRead{0%{transform:translate(-2.4px,.8px)}100%{transform:translate(2.4px,.8px)}}",
  ];

  it.each(KEYFRAMES)("holds the canvas's keyframes %s", (keyframes) => {
    expect(block()).toContain(fold(keyframes));
  });

  // agent-avatars.dc.html lines 33-36 and 46, and the same rules in r1-screens.dc.html: the canvas's selector, the
  // app's (the eye group is `.av-eye`, the iris group `.av-iris-group`, the state is `data-state`) and the animation.
  const STATES: Array<[canvas: string, app: string, animation: string, delayed: boolean]> = [
    [".k-eye.st-idle .ey-eye", '.ui-avatar[data-state="idle"] .av-eye', "lvBlink 4.6s infinite", true],
    [".k-eye.st-idle .ey-iris", '.ui-avatar[data-state="idle"] .av-iris-group', "lvLook 8s ease-in-out infinite", true],
    [".k-eye.st-working .ey-eye", '.ui-avatar[data-state="working"] .av-eye', "lvBlink 6.5s infinite", true],
    [".k-eye.st-working .ey-iris", '.ui-avatar[data-state="working"] .av-iris-group', "lvRead .8s ease-in-out infinite alternate", true],
    [".lv-badge", ".ui-avatar .av-badge", "lvPing 1.1s ease-in-out infinite", false],
  ];

  it.each(STATES)("%s is %s, animated as the canvas animates it", (_canvas, app, animation, delayed) => {
    // Every rule with exactly this selector, as one list of declarations.
    const selector = fold(app).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const decls = [...block().matchAll(new RegExp(`(?:^|\\})${selector}\\{([^}]*)\\}`, "g"))].flatMap((m) => (m[1] ?? "").split(";"));
    expect(decls.length, `${app} has no rule`).toBeGreaterThan(0);
    expect(decls).toContain(`animation:${fold(animation)}`);
    // The canvas offsets each avatar's eye by its own delay, so two avatars on one screen do not blink together.
    if (delayed) expect(decls).toContain("animation-delay:var(--avatar-delay,0s)");
  });

  it("turns the motion off at 16 px (`.lv.sm`) and under reduced motion (design-decisions.md, 9a)", () => {
    expect(block()).toContain(".ui-avatar-sm*{animation:none!important}");
    expect(block()).toContain("@media(prefers-reduced-motion:reduce){.ui-avatar*{animation:none!important}}");
  });

  it("transforms the eye and the iris about their own centre, as `.lv *` does", () => {
    expect(block()).toContain(".ui-avatar.av-eye,.ui-avatar.av-iris-group{transform-box:fill-box;transform-origin:center}");
  });

  it("keeps the motion in the avatar block: no other part of ui.css names an animation", () => {
    const outside = css.slice(0, start) + css.slice(end);
    expect(/@keyframes|animation/.test(outside.replace(/\/\*[\s\S]*?\*\//g, ""))).toBe(false);
  });
});

describe("the avatar's motion state and delay", () => {
  it("draws working with no badge, and names the state on the box", () => {
    const { container } = render(<Avatar agent="orchestrator" size={28} state="working" />);
    const root = container.firstElementChild as HTMLElement;
    expect(root.getAttribute("data-state")).toBe("working");
    expect(container.querySelector(".av-badge")).toBeNull();
  });

  it("sets one delay for one agent at one size, between 0 and -5.9 s in tenths, as 9a's `dl` does", () => {
    const delay = (agent: "orchestrator" | "autopilot" | "research_partner" | "plain", size: 16 | 24 | 28) => {
      const view = render(<Avatar agent={agent} size={size} />);
      const value = (view.container.firstElementChild as HTMLElement).style.getPropertyValue("--avatar-delay");
      view.unmount();
      return value;
    };
    expect(delay("orchestrator", 24)).toBe(delay("orchestrator", 24));
    const at24 = (["orchestrator", "autopilot", "research_partner", "plain"] as const).map((a) => delay(a, 24));
    for (const value of at24) expect(value).toMatch(/^(0|-[0-5](\.\d)?)s$/);
    // The four built-in agents do not blink together in the sidebar.
    expect(new Set(at24).size).toBe(4);
  });
});
