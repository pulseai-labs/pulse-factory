// AC-2 of `r1.s5.w1`: tokens.css holds the canvas values, app-tokens.css names the app's own values with
// the artboard each comes from, and `app.css` and `ui.css` name no raw colour, font size or radius. A
// layout length (a width, a gap, a border width) may be in px. The two token files are excluded by name.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const STYLES = join(__dirname, "..", "styles");
const read = (path: string) => readFileSync(path, "utf8");
const tokens = read(join(STYLES, "tokens.css"));
const appTokens = read(join(STYLES, "app-tokens.css"));
const CHECKED: Array<[name: string, text: string]> = [
  ["app.css", read(join(STYLES, "app.css"))],
  ["ui.css", read(join(__dirname, "..", "ui", "ui.css"))],
  ["shell.css", read(join(__dirname, "..", "shell", "shell.css"))],
  ["add-project.css", read(join(STYLES, "add-project.css"))],
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

/** The value of a custom property in a stylesheet (the last declaration wins). */
function token(css: string, name: string): string | undefined {
  const all = [...stripComments(css).matchAll(new RegExp(`${name}\\s*:\\s*([^;]+);`, "g"))];
  return all.at(-1)?.[1]?.trim();
}

describe("tokens.css holds the canvas values", () => {
  // A table of sentinel tokens from the canvas's token files (tokens/colors, typography, spacing, radii).
  const SENTINELS: Array<[string, string]> = [
    ["--gray-0", "#08090a"],
    ["--gray-12", "#f4f6f8"],
    ["--mint-400", "#0fb573"],
    ["--amber-400", "#ffb020"],
    ["--amber-tint", "rgba(255, 176, 32, 0.12)"],
    ["--violet-400", "#9b87ff"],
    ["--red-400", "#ff4d63"],
    ["--blue-400", "#4d9fff"],
    ["--radius-sm", "4px"],
    ["--radius-md", "6px"],
    ["--radius-full", "999px"],
    ["--text-base", "13px"],
    ["--text-sm", "12px"],
    ["--text-xs", "11px"],
    ["--control-h", "30px"],
    ["--control-h-lg", "38px"],
    ["--tracking-caps", "0.08em"],
  ];

  it.each(SENTINELS)("%s is %s", (name, value) => {
    expect(token(tokens, name)).toBe(value.replace(/\s+/g, " "));
  });

  it("the header names the r1.s5 snapshot and its pin beside the old one", () => {
    const header = tokens.slice(0, tokens.indexOf("---- tokens/colors.css"));
    expect(header).toContain("1794491284-3f16");
    expect(header).toContain("1794067039-9fa3");
  });
});

describe("app-tokens.css names the app's own values", () => {
  const REQUIRED: Array<[string, string]> = [
    ["--text-ui", "12.5px"],
    ["--text-meta", "11.5px"],
    ["--text-tag", "10.5px"],
    ["--radius-btn", "5px"],
    ["--radius-btn-lg", "6px"],
    ["--btn-h-lg", "32px"],
    ["--btn-h-sm", "28px"],
    ["--btn-h-xl", "40px"],
    ["--pill-h", "22px"],
    ["--chip-h", "18px"],
    ["--dot", "7px"],
    ["--amber-border", "rgba(255, 176, 32, 0.28)"],
    ["--avatar-violet", "var(--violet-400)"],
    ["--avatar-red", "var(--red-400)"],
    ["--avatar-mint", "var(--mint-400)"],
    ["--avatar-gray", "var(--gray-9)"],
    // The shell's layout tokens (`r1.s5.w2`, Approach 13).
    ["--titlebar-h", "42px"],
    ["--rail-w", "56px"],
    ["--sidebar-w", "288px"],
    ["--dock-w", "328px"],
    ["--rail-btn", "38px"],
    ["--tab-h", "30px"],
    ["--column-max", "1180px"],
  ];

  it.each(REQUIRED)("%s is %s", (name, value) => {
    expect(token(appTokens, name)).toBe(value);
  });

  it("puts the bundled faces first in the two font stacks", () => {
    expect(token(appTokens, "--font-sans")?.startsWith('"Geist Variable"')).toBe(true);
    expect(token(appTokens, "--font-mono")?.startsWith('"Geist Mono Variable"')).toBe(true);
  });

  it("moves the faces out of app.css", () => {
    const appCss = CHECKED[0]?.[1] ?? "";
    expect(declarations(appCss).some((d) => d.prop === "--font-sans" || d.prop === "--font-mono")).toBe(false);
  });

  it("each token that is not a face carries a comment that names its artboard", () => {
    const ARTBOARD = /\b(2a|3a|3c|3f|6a|9a|11[a-l]|Chat|Pair|Needs you|Agents|Agent Avatars|Retry now)\b/;
    const lines = appTokens.split("\n").filter((l) => /^\s*--[a-z0-9-]+\s*:/.test(l));
    expect(lines.length).toBeGreaterThanOrEqual(REQUIRED.length);
    for (const line of lines) {
      if (/^\s*--font-(sans|mono)\s*:/.test(line)) continue;
      const name = line.trim().split(":")[0];
      const comment = /\/\*([\s\S]*?)\*\//.exec(line)?.[1] ?? "";
      expect(comment.trim().length, `${name} has no comment`).toBeGreaterThan(0);
      // The avatar's colours name the 9a artboard, the rest an artboard of the two screen files.
      expect(ARTBOARD.test(comment) || name?.startsWith("--avatar-") === true, `${name}: no artboard in "${comment}"`).toBe(true);
    }
  });
});

describe("app.css and ui.css use tokens only", () => {
  const COLOUR_FUNCTION = /#[0-9a-f]{3,8}\b|\brgba?\s*\(|\bhsla?\s*\(|\bhwb\s*\(|\boklch\s*\(|\blab\s*\(/i;
  const NAMED_COLOUR = /\b(white|black|red|green|blue|gray|grey|orange|yellow|purple|pink|cyan|magenta|silver|gold|navy|teal|lime|maroon|olive)\b/i;
  const RAW_LENGTH = /(^|[^\w-])-?\d*\.?\d+(px|rem|em|pt|vw|vh)\b/;
  const COLOUR_PROPS = /^(color|background|background-color|border|border-color|border-top|border-right|border-bottom|border-left|border-top-color|border-bottom-color|outline|outline-color|fill|stroke|box-shadow|text-shadow|caret-color|accent-color)$/;

  it.each(CHECKED)("%s names no raw colour", (name, css) => {
    for (const d of declarations(css)) {
      expect(COLOUR_FUNCTION.test(d.value), `${name}: ${d.prop}: ${d.value}`).toBe(false);
      if (COLOUR_PROPS.test(d.prop)) {
        const bare = d.value.replace(/var\(--[a-z0-9-]+\)/gi, "");
        expect(NAMED_COLOUR.test(bare), `${name}: ${d.prop}: ${d.value}`).toBe(false);
      }
    }
  });

  it.each(CHECKED)("%s names no raw font size", (name, css) => {
    for (const d of declarations(css)) {
      if (d.prop !== "font-size" && d.prop !== "font") continue;
      const bare = d.value.replace(/var\([^)]*\)/g, "");
      expect(RAW_LENGTH.test(bare), `${name}: ${d.prop}: ${d.value}`).toBe(false);
    }
  });

  it.each(CHECKED)("%s names no raw radius", (name, css) => {
    for (const d of declarations(css)) {
      if (!d.prop.startsWith("border") || !d.prop.includes("radius")) continue;
      const bare = d.value.replace(/var\([^)]*\)/g, "");
      expect(/\d/.test(bare.replace(/\b0\b/g, "")), `${name}: ${d.prop}: ${d.value}`).toBe(false);
    }
  });

  it.each(CHECKED)("%s names no token that no token file defines", (name, css) => {
    const defined = new Set(
      [...stripComments(tokens + appTokens).matchAll(/(--[a-z0-9-]+)\s*:/gi)].map((m) => m[1]),
    );
    // Set on an element by a component (src/ui/Avatar.tsx), not in a file.
    defined.add("--avatar-c");
    for (const m of stripComments(css).matchAll(/var\(\s*(--[a-z0-9-]+)/gi)) {
      expect(defined.has(m[1] ?? ""), `${name}: var(${m[1]}) has no definition`).toBe(true);
    }
  });

  it("neither file sets a token the token files own", () => {
    for (const [name, css] of CHECKED) {
      expect(declarations(css).filter((d) => d.prop.startsWith("--") && d.prop !== "--avatar-c"), name).toEqual([]);
    }
  });
});

describe("the eyebrow", () => {
  it("uses the tertiary text colour and not the accent", () => {
    const ui = CHECKED[1]?.[1] ?? "";
    const rule = /\.eyebrow\s*\{([^}]*)\}/.exec(stripComments(ui))?.[1] ?? "";
    expect(rule).toContain("color: var(--text-tertiary)");
    expect(rule).not.toContain("--accent");
    const app = CHECKED[0]?.[1] ?? "";
    expect(/\.eyebrow\s*\{[^}]*--accent/.test(stripComments(app))).toBe(false);
  });
});

describe("main.tsx loads the styles in order", () => {
  it("tokens, app tokens, the components, the app, then the shell", () => {
    const main = read(join(__dirname, "..", "main.tsx"));
    const order = ["./styles/tokens.css", "./styles/app-tokens.css", "./ui/ui.css", "./styles/app.css", "./shell/shell.css"].map((p) => main.indexOf(`"${p}"`));
    for (const i of order) expect(i).toBeGreaterThan(-1);
    expect([...order].sort((a, b) => a - b)).toEqual(order);
  });
});
