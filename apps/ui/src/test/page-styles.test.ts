// AC-3 (r1.s5.w4): the three page style files name no raw colour, font size or radius, and no token that no
// token file defines. The rule is the one of `styles.test.ts` (r1.s5.w1) applied to `needs-you.css`,
// `agents.css` and `settings.css`. A layout length (a width, a gap, a border width) may be in px. The pages
// import their own file (the Needs you page, the Agents page and the Settings page).
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const SRC = join(__dirname, "..");
const read = (path: string) => readFileSync(path, "utf8");
const tokens = read(join(SRC, "styles", "tokens.css"));
const appTokens = read(join(SRC, "styles", "app-tokens.css"));
const PAGES: Array<[name: string, text: string, screen: string]> = [
  ["needs-you.css", read(join(SRC, "styles", "needs-you.css")), "screens/NeedsYou.tsx"],
  ["agents.css", read(join(SRC, "styles", "agents.css")), "screens/Agents.tsx"],
  ["settings.css", read(join(SRC, "styles", "settings.css")), "screens/Settings.tsx"],
];

const stripComments = (css: string) => css.replace(/\/\*[\s\S]*?\*\//g, "");

/** Every `property: value` pair of a stylesheet, in order. */
function declarations(css: string): Array<{ prop: string; value: string }> {
  const out: Array<{ prop: string; value: string }> = [];
  for (const m of stripComments(css).matchAll(/([a-z-]+)\s*:\s*([^;{}]+)(?=[;}])/gi)) {
    out.push({ prop: (m[1] ?? "").toLowerCase(), value: (m[2] ?? "").trim() });
  }
  return out;
}

const COLOUR_FUNCTION = /#[0-9a-f]{3,8}\b|\brgba?\s*\(|\bhsla?\s*\(|\bhwb\s*\(|\boklch\s*\(|\blab\s*\(/i;
const NAMED_COLOUR = /\b(white|black|red|green|blue|gray|grey|orange|yellow|purple|pink|cyan|magenta|silver|gold|navy|teal|lime|maroon|olive)\b/i;
const RAW_LENGTH = /(^|[^\w-])-?\d*\.?\d+(px|rem|em|pt|vw|vh)\b/;
const COLOUR_PROPS = /^(color|background|background-color|border|border-color|border-top|border-right|border-bottom|border-left|border-top-color|border-bottom-color|outline|outline-color|fill|stroke|box-shadow|text-shadow|caret-color|accent-color)$/;

describe("the page styles use tokens only", () => {
  it.each(PAGES)("%s names no raw colour", (name, css) => {
    for (const d of declarations(css)) {
      expect(COLOUR_FUNCTION.test(d.value), `${name}: ${d.prop}: ${d.value}`).toBe(false);
      if (COLOUR_PROPS.test(d.prop)) {
        const bare = d.value.replace(/var\(--[a-z0-9-]+\)/gi, "");
        expect(NAMED_COLOUR.test(bare), `${name}: ${d.prop}: ${d.value}`).toBe(false);
      }
    }
  });

  it.each(PAGES)("%s names no raw font size", (name, css) => {
    for (const d of declarations(css)) {
      if (d.prop !== "font-size" && d.prop !== "font") continue;
      const bare = d.value.replace(/var\([^)]*\)/g, "");
      expect(RAW_LENGTH.test(bare), `${name}: ${d.prop}: ${d.value}`).toBe(false);
    }
  });

  it.each(PAGES)("%s names no raw radius", (name, css) => {
    for (const d of declarations(css)) {
      if (!d.prop.startsWith("border") || !d.prop.includes("radius")) continue;
      const bare = d.value.replace(/var\([^)]*\)/g, "");
      expect(/\d/.test(bare.replace(/\b0\b/g, "")), `${name}: ${d.prop}: ${d.value}`).toBe(false);
    }
  });

  it.each(PAGES)("%s names no token that no token file defines", (name, css) => {
    const defined = new Set([...stripComments(tokens + appTokens).matchAll(/(--[a-z0-9-]+)\s*:/gi)].map((m) => m[1]));
    for (const m of stripComments(css).matchAll(/var\(\s*(--[a-z0-9-]+)/gi)) {
      expect(defined.has(m[1] ?? ""), `${name}: var(${m[1]}) has no definition`).toBe(true);
    }
  });

  it.each(PAGES)("%s sets no token the token files own", (name, css) => {
    expect(declarations(css).filter((d) => d.prop.startsWith("--")), name).toEqual([]);
  });

  it.each(PAGES)("%s is imported by its screen", (name, _css, screen) => {
    expect(read(join(SRC, screen))).toContain(`styles/${name}`);
  });
});
