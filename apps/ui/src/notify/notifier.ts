// The notifier (r2.s11.w5, Approach 9): the one small interface beside the transport that shows an operating
// system notification and reports a click. It has two implementations, chosen as the transport is (`main.tsx`):
// this file's browser one, which uses the page's own `Notification` (the test bridge), and `tauri-notifier.ts`,
// which uses the notification plugin inside the Mac app. A test passes its own.
//
// A notification holds the controller's notice text and IDs only, never a seat's words, and nothing of it goes
// to browser storage, the console or a URL.

export type NotifyOutcome = "shown" | "blocked";

/** What the app shows: a title, a body and the request the notification names. */
export interface ShownNotification {
  title: string;
  body: string;
  request: string | null;
}

export interface Notifier {
  /**
   * Shows one notification. Before it shows, it reads the permission and, when that is not decided, asks once in a
   * run of the app. It resolves `blocked` when the permission is not granted or the notification fails.
   */
  show(notification: ShownNotification): Promise<NotifyOutcome>;
  /** Reports a click on a notification with the request it names, or with none. Returns the function that stops listening. */
  onOpen(listener: (request: string | null) => void): () => void;
}

/** The part of the page's `Notification` that the browser notifier uses. A test injects its own. */
export interface NotificationLike {
  new (title: string, options?: { body?: string }): { onclick: ((event: unknown) => void) | null };
  permission: string;
  requestPermission(): Promise<string>;
}

export function createBrowserNotifier(
  notification: NotificationLike | undefined = typeof Notification === "undefined" ? undefined : (Notification as unknown as NotificationLike),
): Notifier {
  const listeners = new Set<(request: string | null) => void>();
  let asked = false;

  async function granted(Ctor: NotificationLike): Promise<boolean> {
    if (Ctor.permission === "granted") return true;
    if (Ctor.permission === "denied") return false;
    // Not decided: ask once in a run of the app.
    if (asked) return false;
    asked = true;
    try {
      return (await Ctor.requestPermission()) === "granted";
    } catch {
      return false;
    }
  }

  return {
    async show(note) {
      if (notification === undefined) return "blocked";
      try {
        if (!(await granted(notification))) return "blocked";
        const shown = new notification(note.title, { body: note.body });
        shown.onclick = () => {
          for (const listener of [...listeners]) listener(note.request);
        };
        return "shown";
      } catch {
        return "blocked";
      }
    },
    onOpen(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
}
