// The state pill: a 22 px round tag with the state's dot. Needs you is amber, working is blue, done and
// merged are green, idle and ended are neutral. The text is the caller's, not a fixed word.
import type { HTMLAttributes, ReactNode } from "react";
import { cx } from "./cx";
import { StateDot, type RunState } from "./StateDot";

export interface StatePillProps extends Omit<HTMLAttributes<HTMLSpanElement>, "className"> {
  state: RunState;
  children?: ReactNode;
}

export function StatePill(props: StatePillProps) {
  const { state, children, ...rest } = props;
  return (
    <span {...rest} className={cx("ui-pill", `ui-pill-${state}`)}>
      <StateDot state={state} />
      {children}
    </span>
  );
}
