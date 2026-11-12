// The small upper-case label above a block, in the tertiary text colour.
import type { HTMLAttributes, ReactNode } from "react";

export function Eyebrow(props: Omit<HTMLAttributes<HTMLParagraphElement>, "className"> & { children?: ReactNode }) {
  const { children, ...rest } = props;
  return (
    <p {...rest} className="eyebrow">
      {children}
    </p>
  );
}
