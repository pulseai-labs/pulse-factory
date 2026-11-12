// The button of the canvas. Only the primary action of a screen is green (`primary`). A default button
// is the elevated grey one, a ghost has no border, `danger` is red text with no fill, and `locked` is
// the dashed, disabled button of a function a later release brings: it carries the lock and takes a
// `title` that says which release (G4).
import type { ButtonHTMLAttributes, ReactNode } from "react";
import { cx } from "./cx";
import { Icon, type IconName } from "./Icon";

export type ButtonVariant = "default" | "primary" | "ghost" | "danger" | "locked";
/** `sm` 28 px, `md` 30 px (the default), `lg` 32 px, `xl` 40 px and full width (the pair card's). */
export type ButtonSize = "sm" | "md" | "lg" | "xl";

export interface ButtonProps extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "className"> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  /** An icon before the label. */
  icon?: IconName;
  children?: ReactNode;
}

export function Button(props: ButtonProps) {
  const { variant = "default", size = "md", icon, children, type = "button", disabled, ...rest } = props;
  const locked = variant === "locked";
  return (
    <button
      {...rest}
      type={type}
      className={cx("ui-btn", `ui-btn-${variant}`, `ui-btn-${size}`)}
      disabled={locked || disabled}
    >
      {locked ? <Icon name="lock" size={14} /> : icon ? <Icon name={icon} size={14} /> : null}
      {children}
    </button>
  );
}
