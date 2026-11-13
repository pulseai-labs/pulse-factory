// The raised card. `needs_you` gives it the amber border of a card that waits for the operator.
import type { HTMLAttributes, ReactNode } from "react";
import { cx } from "./cx";

export interface CardProps extends Omit<HTMLAttributes<HTMLDivElement>, "className"> {
  tone?: "default" | "needs_you";
  children?: ReactNode;
}

export function Card(props: CardProps) {
  const { tone = "default", children, ...rest } = props;
  return (
    <div {...rest} className={cx("ui-card", tone === "needs_you" && "ui-card-needs_you")}>
      {children}
    </div>
  );
}
