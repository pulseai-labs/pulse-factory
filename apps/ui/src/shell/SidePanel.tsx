// The frame of a side panel (11a, 11b, 11e; patch `fix/ui-canvas-motion`). The canvas puts the sidebar and the dock
// each in a wrapper with `flex:none; display:flex; overflow:hidden; width:<open ? W : 0>; transition: width 280ms
// var(--ease-standard)`, and the dock's wrapper also has `justify-content:flex-end`. This is that wrapper. The
// panel inside stays mounted at its own width and the frame clips it, so the main column reflows as the frame's
// width moves (`shell.css`, `.panel`). Reduced motion turns the transition off.
//
// A closed panel is `inert` and `aria-hidden`: out of the tab order and out of the accessibility tree. When it
// closes with the focus inside (⌘B from the sidebar's search field), the focus leaves it, as it did when the panel
// unmounted. The state is in the first commit, so a persisted state draws with no transition on the first paint.
// The shell drops the frame on a focused page and mounts it again after: a page change has no transition.
import { useLayoutEffect, useRef, type ReactNode } from "react";

export type PanelName = "sidebar" | "dock";

export function SidePanel(props: { name: PanelName; open: boolean; children?: ReactNode }) {
  const { name, open, children } = props;
  const ref = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    const frame = ref.current;
    if (open || frame === null) return;
    const focused = document.activeElement;
    if (focused instanceof HTMLElement && frame.contains(focused)) focused.blur();
  }, [open]);

  return (
    <div
      ref={ref}
      className={`panel panel-${name}`}
      data-panel={name}
      data-open={open ? "true" : "false"}
      inert={!open}
      aria-hidden={open ? undefined : true}
    >
      {children}
    </div>
  );
}
