// The notifier inside the Mac app (r2.s11.w5, Approach 9 and 11): the notification plugin's four calls,
// `isPermissionGranted`, `requestPermission`, `sendNotification` and `onAction` (a click reports the action
// `tap`). The module is injected so a test runs no Tauri. The capability file grants these four calls and
// nothing else (`apps/desktop/src-tauri/capabilities/default.json`).
import { isPermissionGranted, onAction, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import type { Notifier, NotifyOutcome } from "./notifier";

/** The part of the plugin that the notifier uses. */
export interface PluginLike {
  isPermissionGranted(): Promise<boolean>;
  requestPermission(): Promise<"granted" | "denied" | "default">;
  sendNotification(options: { title: string; body?: string; extra?: Record<string, unknown> }): void;
  onAction(
    callback: (event: { actionId: string; notification: { extra?: Record<string, unknown> } | null }) => void,
  ): Promise<unknown>;
}

const PLUGIN: PluginLike = { isPermissionGranted, requestPermission, sendNotification, onAction: (cb) => onAction(cb) };

export function createTauriNotifier(plugin: PluginLike = PLUGIN): Notifier {
  const listeners = new Set<(request: string | null) => void>();
  let asked = false;
  let listening: Promise<void> | null = null;

  // Desktop notifications report a click only when a listener existed as they were shown, so set it before the first one.
  function listen(): Promise<void> {
    listening ??= plugin
      .onAction((event) => {
        if (event.actionId !== "tap") return;
        const named = event.notification?.extra?.request;
        const request = typeof named === "string" ? named : null;
        for (const listener of [...listeners]) listener(request);
      })
      .then(
        () => undefined,
        () => undefined,
      );
    return listening;
  }

  return {
    async show(note): Promise<NotifyOutcome> {
      try {
        let granted = await plugin.isPermissionGranted();
        if (!granted && !asked) {
          asked = true;
          granted = (await plugin.requestPermission()) === "granted";
        }
        if (!granted) return "blocked";
        await listen();
        plugin.sendNotification({ title: note.title, body: note.body, extra: { request: note.request } });
        return "shown";
      } catch {
        return "blocked";
      }
    },
    onOpen(listener) {
      listeners.add(listener);
      void listen();
      return () => {
        listeners.delete(listener);
      };
    },
  };
}
