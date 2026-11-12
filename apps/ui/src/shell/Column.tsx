// The centred page column (11e): `max-width` 1180 px by default (the `--column-max` token), padding
// `30px 36px 48px` and a 30 px gap between its children. A page takes another width with `max`.
import type { CSSProperties, ReactNode } from "react";

export function Column(props: { max?: number; children?: ReactNode }) {
  const { max, children } = props;
  const style: CSSProperties | undefined = max === undefined ? undefined : { maxWidth: `${max}px` };
  return (
    <div className="column" style={style}>
      {children}
    </div>
  );
}
