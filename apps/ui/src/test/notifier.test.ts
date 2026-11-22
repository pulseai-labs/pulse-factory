// AC-3 (r2.s11.w5): the two notifiers (Approach 9). The browser one uses the page's own `Notification`; the
// Tauri one uses the notification plugin's four calls through an injected module. Each maps the permission,
// asks at most once in a run of the app, shows a notification with the title and the body it is given, reports
// a click with the request the notification named, and answers `blocked` when the permission is not granted or a
// call fails. No Tauri runs here.
import { describe, expect, it, vi } from "vitest";
import { createBrowserNotifier, type NotificationLike } from "../notify/notifier";
import { createTauriNotifier, type PluginLike } from "../notify/tauri-notifier";

const NOTE = { title: "alpha · u1 asks a question", body: "Unit u1 stopped. R-12", request: "R-12" };

/** A stand-in for the page's `Notification`: it records what it is built with and can be clicked. */
function fakeNotification(permission: "granted" | "denied" | "default", asks: Array<"granted" | "denied" | "default"> = []) {
  const built: Array<{ title: string; options: { body?: string } | undefined; click: () => void }> = [];
  const requestPermission = vi.fn(async () => {
    const next = asks.shift() ?? permission;
    ctor.permission = next;
    return next;
  });
  class Fake {
    static permission = permission;
    static requestPermission = requestPermission;
    onclick: ((event: unknown) => void) | null = null;
    constructor(
      readonly title: string,
      readonly options?: { body?: string },
    ) {
      const self = this;
      built.push({ title, options, click: () => self.onclick?.({}) });
    }
  }
  const ctor = Fake as unknown as NotificationLike & { permission: string };
  return { ctor, built, requestPermission };
}

describe("the browser notifier", () => {
  it("shows a notification with the title and the body when the permission is granted", async () => {
    const { ctor, built } = fakeNotification("granted");
    const notifier = createBrowserNotifier(ctor);
    await expect(notifier.show(NOTE)).resolves.toBe("shown");
    expect(built).toHaveLength(1);
    expect(built[0]?.title).toBe(NOTE.title);
    expect(built[0]?.options?.body).toBe(NOTE.body);
  });

  it("answers `blocked` and builds nothing when the permission is denied", async () => {
    const { ctor, built, requestPermission } = fakeNotification("denied");
    const notifier = createBrowserNotifier(ctor);
    await expect(notifier.show(NOTE)).resolves.toBe("blocked");
    expect(built).toHaveLength(0);
    expect(requestPermission).not.toHaveBeenCalled();
  });

  it("asks once when the permission is not decided, and shows when it is granted", async () => {
    const { ctor, built, requestPermission } = fakeNotification("default", ["granted"]);
    const notifier = createBrowserNotifier(ctor);
    await expect(notifier.show(NOTE)).resolves.toBe("shown");
    await expect(notifier.show(NOTE)).resolves.toBe("shown");
    expect(requestPermission).toHaveBeenCalledTimes(1);
    expect(built).toHaveLength(2);
  });

  it("answers `blocked` when the ask is refused, and never asks a second time in the run", async () => {
    const { ctor, requestPermission } = fakeNotification("default", ["default", "default"]);
    const notifier = createBrowserNotifier(ctor);
    await expect(notifier.show(NOTE)).resolves.toBe("blocked");
    await expect(notifier.show(NOTE)).resolves.toBe("blocked");
    expect(requestPermission).toHaveBeenCalledTimes(1);
  });

  it("answers `blocked` when the page has no `Notification`, and when building one throws", async () => {
    await expect(createBrowserNotifier(undefined).show(NOTE)).resolves.toBe("blocked");
    class Throws {
      static permission = "granted";
      constructor() {
        throw new Error("no notifications here");
      }
    }
    await expect(createBrowserNotifier(Throws as unknown as NotificationLike).show(NOTE)).resolves.toBe("blocked");
  });

  it("reports a click with the request the notification named", async () => {
    const { ctor, built } = fakeNotification("granted");
    const notifier = createBrowserNotifier(ctor);
    const opened: Array<string | null> = [];
    const stop = notifier.onOpen((request) => opened.push(request));
    await notifier.show(NOTE);
    built[0]?.click();
    expect(opened).toEqual(["R-12"]);
    stop();
    built[0]?.click();
    expect(opened).toEqual(["R-12"]);
  });
});

/** A stand-in for `@tauri-apps/plugin-notification`: the four calls the notifier makes. */
function fakePlugin(patch: Partial<PluginLike> = {}) {
  const sent: Array<{ title: string; body?: string; extra?: Record<string, unknown> }> = [];
  let action: ((event: { actionId: string; notification: { extra?: Record<string, unknown> } | null }) => void) | null = null;
  const plugin: PluginLike = {
    isPermissionGranted: vi.fn(async () => true),
    requestPermission: vi.fn(async () => "granted" as const),
    sendNotification: vi.fn((options) => {
      sent.push(options);
    }),
    onAction: vi.fn(async (cb) => {
      action = cb;
      return { unregister: async () => {} };
    }),
    ...patch,
  };
  return { plugin, sent, act: (event: Parameters<NonNullable<typeof action>>[0]) => action?.(event) };
}

describe("the Tauri notifier", () => {
  it("sends the notification through the plugin with the title, the body and the request, and answers `shown`", async () => {
    const { plugin, sent } = fakePlugin();
    await expect(createTauriNotifier(plugin).show(NOTE)).resolves.toBe("shown");
    expect(sent).toEqual([{ title: NOTE.title, body: NOTE.body, extra: { request: "R-12" } }]);
    expect(plugin.requestPermission).not.toHaveBeenCalled();
  });

  it("listens for a click before it shows, and reports the action `tap` with the request the notification named", async () => {
    const { plugin, act } = fakePlugin();
    const notifier = createTauriNotifier(plugin);
    const opened: Array<string | null> = [];
    notifier.onOpen((request) => opened.push(request));
    await notifier.show(NOTE);
    expect(plugin.onAction).toHaveBeenCalledTimes(1);
    act({ actionId: "tap", notification: { extra: { request: "R-12" } } });
    expect(opened).toEqual(["R-12"]);
    // A click that names no request, and an action that is no tap.
    act({ actionId: "tap", notification: null });
    expect(opened).toEqual(["R-12", null]);
    act({ actionId: "other", notification: { extra: { request: "R-13" } } });
    expect(opened).toEqual(["R-12", null]);
  });

  it("asks for the permission once in a run when it is not granted, and shows when it is granted then", async () => {
    // The plugin reads "granted" once the operator allowed it.
    const { plugin, sent } = fakePlugin({ isPermissionGranted: vi.fn().mockResolvedValueOnce(false).mockResolvedValue(true) });
    const notifier = createTauriNotifier(plugin);
    await expect(notifier.show(NOTE)).resolves.toBe("shown");
    await expect(notifier.show(NOTE)).resolves.toBe("shown");
    expect(plugin.requestPermission).toHaveBeenCalledTimes(1);
    expect(sent).toHaveLength(2);
  });

  it("answers `blocked` when the permission is not granted, and asks no second time", async () => {
    const { plugin, sent } = fakePlugin({
      isPermissionGranted: vi.fn(async () => false),
      requestPermission: vi.fn(async () => "denied" as const),
    });
    const notifier = createTauriNotifier(plugin);
    await expect(notifier.show(NOTE)).resolves.toBe("blocked");
    await expect(notifier.show(NOTE)).resolves.toBe("blocked");
    expect(plugin.requestPermission).toHaveBeenCalledTimes(1);
    expect(sent).toHaveLength(0);
  });

  it("answers `blocked` when a call of the plugin rejects", async () => {
    await expect(createTauriNotifier(fakePlugin({ isPermissionGranted: vi.fn(async () => Promise.reject(new Error("no"))) }).plugin).show(NOTE)).resolves.toBe("blocked");
    await expect(createTauriNotifier(fakePlugin({ requestPermission: vi.fn(async () => Promise.reject(new Error("no"))), isPermissionGranted: vi.fn(async () => false) }).plugin).show(NOTE)).resolves.toBe("blocked");
    await expect(
      createTauriNotifier(
        fakePlugin({
          sendNotification: vi.fn(() => {
            throw new Error("no");
          }),
        }).plugin,
      ).show(NOTE),
    ).resolves.toBe("blocked");
  });

  it("still shows when the click listener cannot be set, and reports no click", async () => {
    const { plugin, sent } = fakePlugin({ onAction: vi.fn(async () => Promise.reject(new Error("no listener"))) });
    await expect(createTauriNotifier(plugin).show(NOTE)).resolves.toBe("shown");
    expect(sent).toHaveLength(1);
  });
});
