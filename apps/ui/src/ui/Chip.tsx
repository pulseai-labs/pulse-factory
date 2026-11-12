// A small tag: the billing mark, the worktree and branch chips, a warning such as `unqualified`.
// `red` is for an error mark (a tool call that failed); the canvas's four tones are the rest.
import type { HTMLAttributes, ReactNode } from "react";
import { cx } from "./cx";

export type ChipTone = "neutral" | "violet" | "mint" | "amber" | "red";

export interface ChipProps extends Omit<HTMLAttributes<HTMLSpanElement>, "className"> {
  tone?: ChipTone;
  /** The mono face, as the billing tag and the branch chip use. */
  mono?: boolean;
  children?: ReactNode;
}

export function Chip(props: ChipProps) {
  const { tone = "neutral", mono = false, children, ...rest } = props;
  return (
    <span {...rest} className={cx("ui-chip", `ui-chip-${tone}`, mono && "ui-chip-mono")}>
      {children}
    </span>
  );
}
