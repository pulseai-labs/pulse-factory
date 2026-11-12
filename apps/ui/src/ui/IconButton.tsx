// A square button that holds one icon. Its `title` is required: it is the accessible name and the tooltip.
import type { ButtonHTMLAttributes } from "react";
import { cx } from "./cx";
import { Icon, type IconName } from "./Icon";

export interface IconButtonProps
  extends Omit<ButtonHTMLAttributes<HTMLButtonElement>, "className" | "title" | "children"> {
  icon: IconName;
  title: string;
  /** 26 px (the default) or 28 px. */
  size?: 26 | 28;
}

export function IconButton(props: IconButtonProps) {
  const { icon, title, size = 26, type = "button", ...rest } = props;
  return (
    <button {...rest} type={type} title={title} aria-label={title} className={cx("ui-icon-btn", `ui-icon-btn-${size}`)}>
      <Icon name={icon} size={16} />
    </button>
  );
}
