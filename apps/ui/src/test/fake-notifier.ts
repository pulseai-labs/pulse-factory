// A notifier for the tests (r2.s11.w5, Approach 9): it records what the app shows, answers the outcome the test
// sets, and reports a click when the test presses one.
import type { Notifier, NotifyOutcome, ShownNotification } from "../notify/notifier";

export class FakeNotifier implements Notifier {
  readonly shown: ShownNotification[] = [];
  /** What `show` answers. `"throw"` makes it reject. */
  outcome: NotifyOutcome | "throw" = "shown";
  private listeners = new Set<(request: string | null) => void>();

  async show(notification: ShownNotification): Promise<NotifyOutcome> {
    if (this.outcome === "throw") throw new Error("the notification failed");
    if (this.outcome === "shown") this.shown.push(notification);
    return this.outcome;
  }

  onOpen(listener: (request: string | null) => void): () => void {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  /** The operator clicks a notification that names `request`, or none. */
  click(request: string | null): void {
    for (const listener of [...this.listeners]) listener(request);
  }
}
