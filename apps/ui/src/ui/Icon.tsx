// The icon set, in the Lucide idiom of the design system: 24×24 view box, stroke 2, round caps and
// joins, `currentColor`. The names are the canvas's. `folder` and `bell` stay in the set for later
// screens, and `arrow-left` is one the app shows that the canvas list does not name. `r1.s5.w2` adds the
// five names that frames 11a to 11l use (`more`, `trash`, `square`, `minus`, `alert`) and the sizes 11 and
// 12, so no later item edits this file. `r1.s6.w1` adds the size 22 (the pair tile's icon, 11j). The shapes are inline (icon-nodes.ts): no package, no fetch.
import { createElement } from "react";
import { ICON_NODES } from "./icon-nodes";

export const ICON_NAMES = Object.keys(ICON_NODES) as ReadonlyArray<keyof typeof ICON_NODES>;
export type IconName = keyof typeof ICON_NODES;

/** The sizes the canvas draws an icon at: 11 and 12 px (the branch line, the tab's close), 13 to 18 px, and 22 px (the pair tile). */
export type IconSize = 11 | 12 | 13 | 14 | 15 | 16 | 17 | 18 | 22;

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
