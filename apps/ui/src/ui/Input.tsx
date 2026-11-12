// A text field at the control height, with the focus ring of the design system. `mono` is for codes and paths.
import type { InputHTMLAttributes } from "react";
import { cx } from "./cx";

export interface InputProps extends Omit<InputHTMLAttributes<HTMLInputElement>, "className"> {
  mono?: boolean;
}

export function Input(props: InputProps) {
  const { mono = false, ...rest } = props;
  return <input {...rest} className={cx("ui-input", mono && "ui-input-mono")} />;
}
