// The body of the dock. In `r1.s5.w4` it renders the Needs you cards in the dock's width (`NeedsYouDock`, 11d):
// the same hooks and Hold as the Needs you page, the same one-press acts. `Dock.tsx` renders whatever this file
// renders.
import { NeedsYouDock } from "../screens/NeedsYou";

export function DockRequests() {
  return <NeedsYouDock />;
}
