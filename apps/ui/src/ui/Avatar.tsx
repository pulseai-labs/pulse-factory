// The agent avatar of the canvas's approved 9a, "Head with one eye": a coloured head of one of eight
// shapes in a 40×40 box, with one eye in a dark socket. The geometry is ported from
// docs/specs/r1/r1.s5-…/canvas/agent-avatars.dc.html: `rr` (line 153) and `BOTS` (the head bodies, lines
// 155–164), `BOT_EYE` (the eye of each head, line 176), the `boteye` branch of `avatar()` (lines 198–203)
// with its markup (the `isEye` block of the 9a template, line 60) and the picks of 9a (line 222).
// `avatar.test.tsx` checks every head against that source.
//
// No animation in this item: the eye sits in its idle place, and `needs_you` only adds the amber badge.
// The canvas's blink, look and read animations come later, with the reduced-motion and 16 px rules.
import type { CSSProperties } from "react";
import { cx } from "./cx";

/** The eight head shapes of 9a. */
export type HeadId = "block" | "dome" | "capsule" | "octa" | "wide" | "antenna" | "ears" | "tall";

interface Head {
  /** The path of the head. */
  body: string;
  /** The path of the ears, for the one head that has them. */
  ears?: string;
  /** The antenna: a stalk and a ball above the head. */
  antenna?: true;
  /** The eye's centre x, centre y and radius. */
  eye: readonly [number, number, number];
}

export const HEADS: Record<HeadId, Head> = {
  block: {
    body: "M13 5h14a8 8 0 0 1 8 8v14a8 8 0 0 1 -8 8h-14a8 8 0 0 1 -8 -8v-14a8 8 0 0 1 8 -8Z",
    eye: [20, 18, 7.5],
  },
  dome: {
    body: "M5 35V20a15 15 0 0 1 30 0v15Z",
    eye: [20, 22, 7.5],
  },
  capsule: {
    body: "M20 3h0a12 12 0 0 1 12 12v10a12 12 0 0 1 -12 12h0a12 12 0 0 1 -12 -12v-10a12 12 0 0 1 12 -12Z",
    eye: [20, 17, 7.5],
  },
  octa: {
    body: "M14 4h12l10 10v12L26 36H14L4 26V14Z",
    eye: [20, 19, 8],
  },
  wide: {
    body: "M10 9h20a8 8 0 0 1 8 8v9a8 8 0 0 1 -8 8h-20a8 8 0 0 1 -8 -8v-9a8 8 0 0 1 8 -8Z",
    eye: [20, 21, 7],
  },
  antenna: {
    body: "M12 11h16a7 7 0 0 1 7 7v11a7 7 0 0 1 -7 7h-16a7 7 0 0 1 -7 -7v-11a7 7 0 0 1 7 -7Z",
    antenna: true,
    eye: [20, 23, 7.5],
  },
  ears: {
    body: "M15 6h10a8 8 0 0 1 8 8v12a8 8 0 0 1 -8 8h-10a8 8 0 0 1 -8 -8v-12a8 8 0 0 1 8 -8Z",
    ears: "M4 15h2a2 2 0 0 1 2 2v6a2 2 0 0 1 -2 2h-2a2 2 0 0 1 -2 -2v-6a2 2 0 0 1 2 -2ZM34 15h2a2 2 0 0 1 2 2v6a2 2 0 0 1 -2 2h-2a2 2 0 0 1 -2 -2v-6a2 2 0 0 1 2 -2Z",
    eye: [20, 19, 7.5],
  },
  tall: {
    body: "M16 3h8a7 7 0 0 1 7 7v20a7 7 0 0 1 -7 7h-8a7 7 0 0 1 -7 -7v-20a7 7 0 0 1 7 -7Z",
    eye: [20, 15, 7],
  },
};

/** The four built-in agents: their colour (an app token) and their head (9a's `picks`). */
export const BUILT_IN_AGENTS = {
  orchestrator: { color: "var(--avatar-violet)", head: "antenna" },
  autopilot: { color: "var(--avatar-red)", head: "dome" },
  research_partner: { color: "var(--avatar-mint)", head: "capsule" },
  plain: { color: "var(--avatar-gray)", head: "block" },
} as const satisfies Record<string, { color: string; head: HeadId }>;

export type BuiltInAgent = keyof typeof BUILT_IN_AGENTS;
/** A later custom agent: any CSS colour and one of the eight heads. */
export interface CustomAgent {
  color: string;
  head: HeadId;
}

/**
 * The sizes the canvas draws an avatar at: 16 (tabs), 20, 22, 24 (sidebar rows), 26, 28 (cards), 40 and
 * 64 (the 11g identity band). The 11a to 11l markup draws 22 px boxes 47 times and one 64 px box.
 */
export type AvatarSize = 16 | 20 | 22 | 24 | 26 | 28 | 40 | 64;

/**
 * The look of the agent with this ID. The four built-in IDs (`orchestrator`, `autopilot`,
 * `research_partner`, `plain`) have their own head and colour; any other ID draws as `plain`.
 */
export function agentLook(agentId: string): BuiltInAgent {
  return agentId in BUILT_IN_AGENTS ? (agentId as BuiltInAgent) : "plain";
}

export interface AvatarProps {
  agent: BuiltInAgent | CustomAgent;
  size: AvatarSize;
  state?: "idle" | "needs_you";
}

export function Avatar(props: AvatarProps) {
  const { agent, size, state = "idle" } = props;
  const { color, head: headId } = typeof agent === "string" ? BUILT_IN_AGENTS[agent] : agent;
  const head = HEADS[headId];
  const [ex, ey, er] = head.eye;
  const style = { width: size, height: size, "--avatar-c": color } as CSSProperties;
  return (
    <span
      className={cx("ui-avatar", size <= 18 && "ui-avatar-sm")}
      style={style}
      data-head={headId}
      data-agent={typeof agent === "string" ? agent : undefined}
      data-state={state}
    >
      <svg viewBox="0 0 40 40" width="100%" height="100%" aria-hidden="true" focusable="false">
        {head.antenna ? (
          <>
            <path className="av-stalk" d="M20 11V6" />
            <circle className="av-ball" cx={20} cy={4.5} r={2.4} />
          </>
        ) : null}
        {head.ears ? <path className="av-fill" d={head.ears} /> : null}
        <path className="av-fill" d={head.body} />
        <circle className="av-socket" cx={ex} cy={ey} r={er + 2.2} />
        <g className="av-eye">
          <circle className="av-sclera" cx={ex} cy={ey} r={er} />
          <g className="av-iris-group">
            <circle className="av-iris" cx={ex} cy={ey} r={er * 0.56} />
            <circle className="av-pupil" cx={ex} cy={ey} r={er * 0.27} />
            <circle className="av-glint" cx={ex + er * 0.2} cy={ey - er * 0.22} r={er * 0.11} />
          </g>
        </g>
        {state === "needs_you" ? <circle className="av-badge" cx={33.5} cy={6.5} r={5} /> : null}
      </svg>
    </span>
  );
}
