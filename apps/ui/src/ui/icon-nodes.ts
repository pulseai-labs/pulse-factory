// The drawing data of the icon set: the paths and shapes of Lucide (https://lucide.dev, ISC licence, with
// the Feather-derived icons under MIT; both texts are in public/licenses/lucide-LICENSE.txt and named in
// public/THIRD-PARTY-NOTICES.md). The data is copied from lucide-react 1.47.0, one entry for each name the
// canvas uses, so the app depends on no icon package and the bundle fetches nothing for them. The keys
// are the design system's names; the comment on each entry names the Lucide icon it is.
export type IconShape = readonly [tag: "path" | "circle" | "rect" | "line", attrs: Readonly<Record<string, string>>];

export const ICON_NODES = {
  /* lucide: activity */
  activity: [
    ["path", {"d":"M22 12h-2.48a2 2 0 0 0-1.93 1.46l-2.35 8.36a.25.25 0 0 1-.48 0L9.24 2.18a.25.25 0 0 0-.48 0l-2.35 8.36A2 2 0 0 1 4.49 12H2"}],
  ],
  /* lucide: circle-alert */
  alert: [
    ["circle", {"cx":"12","cy":"12","r":"10"}],
    ["line", {"x1":"12","x2":"12","y1":"8","y2":"12"}],
    ["line", {"x1":"12","x2":"12.01","y1":"16","y2":"16"}],
  ],
  /* lucide: archive */
  archive: [
    ["rect", {"width":"20","height":"5","x":"2","y":"3","rx":"1"}],
    ["path", {"d":"M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8"}],
    ["path", {"d":"M10 12h4"}],
  ],
  /* lucide: arrow-left */
  "arrow-left": [
    ["path", {"d":"m12 19-7-7 7-7"}],
    ["path", {"d":"M19 12H5"}],
  ],
  /* lucide: arrow-right */
  "arrow-right": [
    ["path", {"d":"M5 12h14"}],
    ["path", {"d":"m12 5 7 7-7 7"}],
  ],
  /* lucide: chart-no-axes-column-increasing */
  "bar-chart": [
    ["path", {"d":"M5 21v-6"}],
    ["path", {"d":"M12 21V9"}],
    ["path", {"d":"M19 21V3"}],
  ],
  /* lucide: bell */
  bell: [
    ["path", {"d":"M10.268 21a2 2 0 0 0 3.464 0"}],
    ["path", {"d":"M3.262 15.326A1 1 0 0 0 4 17h16a1 1 0 0 0 .74-1.673C19.41 13.956 18 12.499 18 8A6 6 0 0 0 6 8c0 4.499-1.411 5.956-2.738 7.326"}],
  ],
  /* lucide: bot */
  bot: [
    ["path", {"d":"M12 8V4H8"}],
    ["rect", {"width":"16","height":"12","x":"4","y":"8","rx":"2"}],
    ["path", {"d":"M2 14h2"}],
    ["path", {"d":"M20 14h2"}],
    ["path", {"d":"M15 13v2"}],
    ["path", {"d":"M9 13v2"}],
  ],
  /* lucide: message-square */
  chat: [
    ["path", {"d":"M22 17a2 2 0 0 1-2 2H6.828a2 2 0 0 0-1.414.586l-2.202 2.202A.71.71 0 0 1 2 21.286V5a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2z"}],
  ],
  /* lucide: check */
  check: [
    ["path", {"d":"M20 6 9 17l-5-5"}],
  ],
  /* lucide: chevron-down */
  "chevron-down": [
    ["path", {"d":"m6 9 6 6 6-6"}],
  ],
  /* lucide: chevron-left */
  "chevron-left": [
    ["path", {"d":"m15 18-6-6 6-6"}],
  ],
  /* lucide: chevron-right */
  "chevron-right": [
    ["path", {"d":"m9 18 6-6-6-6"}],
  ],
  /* lucide: chevrons-up-down */
  "chevrons-up-down": [
    ["path", {"d":"m7 15 5 5 5-5"}],
    ["path", {"d":"m7 9 5-5 5 5"}],
  ],
  /* lucide: circle-dot */
  "circle-dot": [
    ["circle", {"cx":"12","cy":"12","r":"1"}],
    ["circle", {"cx":"12","cy":"12","r":"10"}],
  ],
  /* lucide: clock */
  clock: [
    ["circle", {"cx":"12","cy":"12","r":"10"}],
    ["path", {"d":"M12 6v6l4 2"}],
  ],
  /* lucide: copy */
  copy: [
    ["rect", {"width":"14","height":"14","x":"8","y":"8","rx":"2","ry":"2"}],
    ["path", {"d":"M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"}],
  ],
  /* lucide: folder */
  folder: [
    ["path", {"d":"M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"}],
  ],
  /* lucide: git-branch */
  "git-branch": [
    ["path", {"d":"M15 6a9 9 0 0 0-9 9V3"}],
    ["circle", {"cx":"18","cy":"6","r":"3"}],
    ["circle", {"cx":"6","cy":"18","r":"3"}],
  ],
  /* lucide: layers */
  layers: [
    ["path", {"d":"M12.83 2.18a2 2 0 0 0-1.66 0L2.6 6.08a1 1 0 0 0 0 1.83l8.58 3.91a2 2 0 0 0 1.66 0l8.58-3.9a1 1 0 0 0 0-1.83z"}],
    ["path", {"d":"M2 12a1 1 0 0 0 .58.91l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9A1 1 0 0 0 22 12"}],
    ["path", {"d":"M2 17a1 1 0 0 0 .58.91l8.6 3.91a2 2 0 0 0 1.65 0l8.58-3.9A1 1 0 0 0 22 17"}],
  ],
  /* lucide: lock */
  lock: [
    ["rect", {"width":"18","height":"11","x":"3","y":"11","rx":"2","ry":"2"}],
    ["path", {"d":"M7 11V7a5 5 0 0 1 10 0v4"}],
  ],
  /* lucide: minus */
  minus: [
    ["path", {"d":"M5 12h14"}],
  ],
  /* lucide: ellipsis */
  more: [
    ["circle", {"cx":"12","cy":"12","r":"1"}],
    ["circle", {"cx":"19","cy":"12","r":"1"}],
    ["circle", {"cx":"5","cy":"12","r":"1"}],
  ],
  /* lucide: pause */
  pause: [
    ["rect", {"x":"14","y":"3","width":"5","height":"18","rx":"1"}],
    ["rect", {"x":"5","y":"3","width":"5","height":"18","rx":"1"}],
  ],
  /* lucide: play */
  play: [
    ["path", {"d":"M5 5a2 2 0 0 1 3.008-1.728l11.997 6.998a2 2 0 0 1 .003 3.458l-12 7A2 2 0 0 1 5 19z"}],
  ],
  /* lucide: plus */
  plus: [
    ["path", {"d":"M5 12h14"}],
    ["path", {"d":"M12 5v14"}],
  ],
  /* lucide: refresh-cw */
  refresh: [
    ["path", {"d":"M3 12a9 9 0 0 1 9-9 9.75 9.75 0 0 1 6.74 2.74L21 8"}],
    ["path", {"d":"M21 3v5h-5"}],
    ["path", {"d":"M21 12a9 9 0 0 1-9 9 9.75 9.75 0 0 1-6.74-2.74L3 16"}],
    ["path", {"d":"M8 16H3v5"}],
  ],
  /* lucide: search */
  search: [
    ["path", {"d":"m21 21-4.34-4.34"}],
    ["circle", {"cx":"11","cy":"11","r":"8"}],
  ],
  /* lucide: server */
  server: [
    ["rect", {"width":"20","height":"8","x":"2","y":"2","rx":"2","ry":"2"}],
    ["rect", {"width":"20","height":"8","x":"2","y":"14","rx":"2","ry":"2"}],
    ["line", {"x1":"6","x2":"6.01","y1":"6","y2":"6"}],
    ["line", {"x1":"6","x2":"6.01","y1":"18","y2":"18"}],
  ],
  /* lucide: settings */
  settings: [
    ["path", {"d":"M9.671 4.136a2.34 2.34 0 0 1 4.659 0 2.34 2.34 0 0 0 3.319 1.915 2.34 2.34 0 0 1 2.33 4.033 2.34 2.34 0 0 0 0 3.831 2.34 2.34 0 0 1-2.33 4.033 2.34 2.34 0 0 0-3.319 1.915 2.34 2.34 0 0 1-4.659 0 2.34 2.34 0 0 0-3.32-1.915 2.34 2.34 0 0 1-2.33-4.033 2.34 2.34 0 0 0 0-3.831A2.34 2.34 0 0 1 6.35 6.051a2.34 2.34 0 0 0 3.319-1.915"}],
    ["circle", {"cx":"12","cy":"12","r":"3"}],
  ],
  /* lucide: sliders-horizontal */
  sliders: [
    ["path", {"d":"M10 5H3"}],
    ["path", {"d":"M12 19H3"}],
    ["path", {"d":"M14 3v4"}],
    ["path", {"d":"M16 17v4"}],
    ["path", {"d":"M21 12h-9"}],
    ["path", {"d":"M21 19h-5"}],
    ["path", {"d":"M21 5h-7"}],
    ["path", {"d":"M8 10v4"}],
    ["path", {"d":"M8 12H3"}],
  ],
  /* lucide: sparkles */
  sparkles: [
    ["path", {"d":"M11.017 2.814a1 1 0 0 1 1.966 0l1.051 5.558a2 2 0 0 0 1.594 1.594l5.558 1.051a1 1 0 0 1 0 1.966l-5.558 1.051a2 2 0 0 0-1.594 1.594l-1.051 5.558a1 1 0 0 1-1.966 0l-1.051-5.558a2 2 0 0 0-1.594-1.594l-5.558-1.051a1 1 0 0 1 0-1.966l5.558-1.051a2 2 0 0 0 1.594-1.594z"}],
    ["path", {"d":"M20 2v4"}],
    ["path", {"d":"M22 4h-4"}],
    ["circle", {"cx":"4","cy":"20","r":"2"}],
  ],
  /* lucide: square */
  square: [
    ["rect", {"width":"18","height":"18","x":"3","y":"3","rx":"2"}],
  ],
  /* lucide: trash-2 */
  trash: [
    ["path", {"d":"M10 11v6"}],
    ["path", {"d":"M14 11v6"}],
    ["path", {"d":"M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"}],
    ["path", {"d":"M3 6h18"}],
    ["path", {"d":"M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"}],
  ],
  /* lucide: upload */
  upload: [
    ["path", {"d":"M12 3v12"}],
    ["path", {"d":"m17 8-5-5-5 5"}],
    ["path", {"d":"M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"}],
  ],
  /* lucide: x */
  x: [
    ["path", {"d":"M18 6 6 18"}],
    ["path", {"d":"m6 6 12 12"}],
  ],
  /* lucide: zap */
  zap: [
    ["path", {"d":"M15.914 4a1.5 1.5 0 00-2.474-1.561l-9 9A1.5 1.5 0 005.5 14h4.002a.5.5 0 01.471.666L8.086 20a1.5 1.5 0 002.475 1.56l9-9A1.5 1.5 0 0018.5 10h-3.997a.5.5 0 01-.472-.667z"}],
  ],
  // ---- r1.s6.w2: the icons this item adds go between these two lines, and nowhere else in the file. ----
  /* lucide: paperclip */
  paperclip: [
    ["path", {"d":"m16 6-8.414 8.586a2 2 0 0 0 2.829 2.829l8.414-8.586a4 4 0 1 0-5.657-5.657l-8.379 8.551a6 6 0 1 0 8.485 8.485l8.379-8.551"}],
  ],
  /* lucide: triangle-alert */
  "triangle-alert": [
    ["path", {"d":"m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"}],
    ["path", {"d":"M12 9v4"}],
    ["path", {"d":"M12 17h.01"}],
  ],
  // ---- end r1.s6.w2 ----
  // ---- r1.s6.w3: the icons this item adds go between these two lines, and nowhere else in the file. ----
  /* lucide: laptop */
  laptop: [
    ["path", {"d":"M18 5a2 2 0 0 1 2 2v8.526a2 2 0 0 0 .212.897l1.068 2.127a1 1 0 0 1-.9 1.45H3.62a1 1 0 0 1-.9-1.45l1.068-2.127A2 2 0 0 0 4 15.526V7a2 2 0 0 1 2-2z"}],
    ["path", {"d":"M20.054 15.987H3.946"}],
  ],
  // ---- end r1.s6.w3 ----
  // ---- r1.s6.w4: the icons this item adds go between these two lines, and nowhere else in the file. ----
  /* lucide: house */
  house: [
    ["path", {"d":"M15 21v-8a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1v8"}],
    ["path", {"d":"M3 10a2 2 0 0 1 .709-1.528l7-6a2 2 0 0 1 2.582 0l7 6A2 2 0 0 1 21 10v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"}],
  ],
  // ---- end r1.s6.w4 ----
} as const satisfies Record<string, readonly IconShape[]>;
