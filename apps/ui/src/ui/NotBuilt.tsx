// A function that the app does not build yet (11l). It stays where it will live, looks like itself and is
// dimmed, with a dashed "Later release" tag. It never opens anything and needs no hover to explain
// itself: each form is `aria-disabled`, takes no focus, sets no timer and makes no call. The tag names
// no release number (11l: "Operator decided: Releases are not numbered").
//
// Six forms: `item` (a menu row), `button` (a dashed button), `rail` (a rail icon with the compact tag),
// `segment` (the second half of a switch), `field` (a dimmed text field that takes no text) and `row` (a
// full-width settings row with a description and an off switch).
import { cx } from "./cx";
import { Icon, type IconName } from "./Icon";

/** The 11l tag: "Later release", or "Later" in the rail's compact form. */
export function LaterTag(props: { compact?: boolean }) {
  const { compact = false } = props;
  return <span className={cx("ui-later", compact && "ui-later-compact")}>{compact ? "Later" : "Later release"}</span>;
}

export type NotBuiltForm = "item" | "button" | "rail" | "segment" | "field" | "row";

export interface NotBuiltProps {
  as: NotBuiltForm;
  /** What the function is called. */
  label: string;
  /** The icon before the label (item, button, rail). */
  icon?: IconName;
  /** The second line of a `row`, or the hint of a `field`. */
  description?: string;
  /** `rail` only: draw a ring with its track only (the usage ring) in place of an icon. */
  ring?: boolean;
  /** `button` only: the button is an icon with no visible label, as the pair screen's New tab. */
  compact?: boolean;
}

export function NotBuilt(props: NotBuiltProps) {
  const { as, label, icon, description, ring = false, compact = false } = props;
  const root = { "aria-disabled": true, "data-not-built": as } as const;
  switch (as) {
    case "item":
      return (
        <div {...root} className="nb nb-item">
          {icon ? <Icon name={icon} size={15} /> : null}
          <span className="nb-label">{label}</span>
          <LaterTag />
        </div>
      );
    case "button":
      return (
        <span {...root} className={cx("nb nb-button", compact && "nb-button-compact")}>
          {icon ? <Icon name={icon} size={14} /> : null}
          <span className={cx("nb-label", compact && "ui-sr-only")}>{label}</span>
          {compact ? <span className="ui-sr-only">Later release</span> : <LaterTag />}
        </span>
      );
    case "rail":
      return (
        <div {...root} className="nb nb-rail">
          {ring ? <span className="nb-ring" aria-hidden="true" /> : icon ? <Icon name={icon} size={18} /> : null}
          <span className="ui-sr-only">{label}</span>
          <span className="nb-rail-tag">
            <LaterTag compact />
          </span>
        </div>
      );
    case "segment":
      return (
        <span {...root} className="nb nb-segment">
          {label}
          <LaterTag />
        </span>
      );
    case "field":
      return (
        <div {...root} className="nb nb-field">
          <span className="nb-field-label">{label}</span>
          <span className="nb-field-box">
            <span className="nb-field-hint">{description ?? ""}</span>
            <LaterTag />
          </span>
        </div>
      );
    case "row":
      return (
        <div {...root} className="nb nb-row">
          <span className="nb-row-text">
            <span className="nb-label">{label}</span>
            {description ? <span className="nb-row-description">{description}</span> : null}
          </span>
          <span className="nb-switch" aria-hidden="true" />
          <LaterTag />
        </div>
      );
  }
}
