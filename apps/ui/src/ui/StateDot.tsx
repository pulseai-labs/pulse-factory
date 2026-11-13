// The 7 px state mark. `idle` and `ended` are the hollow neutral dot.
import { cx } from "./cx";

export type RunState = "needs_you" | "working" | "done" | "merged" | "idle" | "ended";

export function StateDot(props: { state: RunState }) {
  return <span className={cx("ui-dot", `ui-dot-${props.state}`)} aria-hidden="true" />;
}
