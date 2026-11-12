// A native pop-up (`<select>`) with the canvas border and a chevron. The pop-up itself stays native.
import type { SelectHTMLAttributes } from "react";
import { Icon } from "./Icon";

export type SelectProps = Omit<SelectHTMLAttributes<HTMLSelectElement>, "className">;

export function Select(props: SelectProps) {
  const { children, ...rest } = props;
  return (
    <span className="ui-select">
      <select {...rest} className="ui-select-field">
        {children}
      </select>
      <span className="ui-select-chevron" aria-hidden="true">
        <Icon name="chevron-down" size={14} />
      </span>
    </span>
  );
}
