// The icon set, in the Lucide idiom of the design system: 24×24 view box, stroke 2, round caps and
// joins, `currentColor`. The names are the canvas's. `folder` stays in the set because the Projects rail
// shows it until `r1.s5.w2` swaps it for `layers`, and `arrow-left` and `bell` are two icons the app
// shows that the canvas list does not name. The shapes are inline (icon-nodes.ts): no package, no fetch.
import { createElement } from "react";
import { ICON_NODES } from "./icon-nodes";

export const ICON_NAMES = Object.keys(ICON_NODES) as ReadonlyArray<keyof typeof ICON_NODES>;
export type IconName = keyof typeof ICON_NODES;

/** The sizes the canvas draws an icon at, 13 to 18 px. */
export type IconSize = 13 | 14 | 15 | 16 | 17 | 18;

export function Icon(props: { name: IconName; size?: IconSize }) {
  const { name, size = 16 } = props;
  return (
    <svg
      className="ui-icon"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={2}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
      data-icon={name}
    >
      {ICON_NODES[name].map(([tag, attrs], i) => createElement(tag, { key: i, ...attrs }))}
    </svg>
  );
}
