// AC-1 of `r1.s5.w1`: the shared components in src/ui/ render the canvas classes, `Button` `locked`
// is disabled and shows the lock, `CountBadge` hides at zero, `StatePill` maps each state to its
// colour class, the `Icon` union is the canvas's list plus three icons, and no component calls the
// transport, sets a timer or touches storage.
import { render, screen } from "@testing-library/react";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import {
  Button,
  Card,
  Chip,
  CountBadge,
  Eyebrow,
  Icon,
  ICON_NAMES,
  IconButton,
  Input,
  Select,
  StateDot,
  StatePill,
} from "../ui";

// The icon names the two canvas screen files use (`r1.s5.w1` spec, "The canvas").
const CANVAS_ICONS = [
  "activity", "archive", "arrow-right", "bar-chart", "bot", "chat", "check", "chevron-down",
  "chevron-left", "chevron-right", "chevrons-up-down", "circle-dot", "clock", "copy", "git-branch",
  "layers", "lock", "pause", "play", "plus", "refresh", "search", "settings", "sliders", "sparkles",
  "upload", "x", "zap",
  // Round 2 adds the five names that frames 11a to 11l use and w1's set lacked (Approach 11).
  "more", "trash", "square", "minus", "alert",
  // `r1.s6.w1` adds the server icon of 11h and 11k.
  "server",
  "laptop",
];
// The three icons the app shows at 41a6b79 that the canvas list lacks.
const APP_ICONS = ["arrow-left", "bell", "folder"];

describe("Button", () => {
  it("renders the default variant at the default size, as a button that does not submit", () => {
    render(<Button>Hold</Button>);
    const button = screen.getByRole("button", { name: "Hold" });
    expect(button.className).toContain("ui-btn");
    expect(button.className).toContain("ui-btn-default");
    expect(button.className).toContain("ui-btn-md");
    expect(button).toHaveProperty("type", "button");
  });

  it.each(["default", "primary", "ghost", "danger"] as const)("renders the %s variant", (variant) => {
    render(<Button variant={variant}>{variant}</Button>);
    expect(screen.getByRole("button", { name: variant }).className).toContain(`ui-btn-${variant}`);
  });

  it.each(["sm", "md", "lg", "xl"] as const)("renders the %s size", (size) => {
    render(<Button size={size}>{size}</Button>);
    expect(screen.getByRole("button", { name: size }).className).toContain(`ui-btn-${size}`);
  });

  it("puts the icon before the label", () => {
    render(<Button icon="plus">Add</Button>);
    const button = screen.getByRole("button", { name: "Add" });
    const first = button.firstElementChild;
    expect(first?.getAttribute("data-icon")).toBe("plus");
    expect(button.textContent).toBe("Add");
  });

  it("locked is disabled, shows the lock, and keeps its title", () => {
    render(
      <Button variant="locked" title="Comes with Release 2">
        Create agent
      </Button>,
    );
    const button = screen.getByRole("button", { name: "Create agent" });
    expect(button).toHaveProperty("disabled", true);
    expect(button.className).toContain("ui-btn-locked");
    expect(button.querySelector("[data-icon='lock']")).not.toBeNull();
    expect(button.getAttribute("title")).toBe("Comes with Release 2");
  });

  it("a locked button never runs its handler", () => {
    let clicks = 0;
    render(
      <Button variant="locked" onClick={() => (clicks += 1)}>
        Locked
      </Button>,
    );
    screen.getByRole("button", { name: "Locked" }).click();
    expect(clicks).toBe(0);
  });

  it("passes disabled and onClick through", () => {
    let clicks = 0;
    render(<Button onClick={() => (clicks += 1)}>Go</Button>);
    screen.getByRole("button", { name: "Go" }).click();
    expect(clicks).toBe(1);
    render(<Button disabled>Off</Button>);
    expect(screen.getByRole("button", { name: "Off" })).toHaveProperty("disabled", true);
  });
});

describe("IconButton", () => {
  it("takes its accessible name from the title", () => {
    render(<IconButton icon="arrow-left" title="Back" />);
    const button = screen.getByRole("button", { name: "Back" });
    expect(button.getAttribute("title")).toBe("Back");
    expect(button.className).toContain("ui-icon-btn");
    expect(button.querySelector("[data-icon='arrow-left']")).not.toBeNull();
  });

  it("is 26 px by default and 28 px on request", () => {
    render(
      <>
        <IconButton icon="x" title="Close" />
        <IconButton icon="x" title="Close big" size={28} />
      </>,
    );
    expect(screen.getByRole("button", { name: "Close" }).className).toContain("ui-icon-btn-26");
    expect(screen.getByRole("button", { name: "Close big" }).className).toContain("ui-icon-btn-28");
  });

  it("is disabled when asked", () => {
    render(<IconButton icon="x" title="Close" disabled />);
    expect(screen.getByRole("button", { name: "Close" })).toHaveProperty("disabled", true);
  });
});

describe("Input and Select", () => {
  it("Input keeps its props and takes the mono face on request", () => {
    render(
      <>
        <Input aria-label="Plain" defaultValue="a" />
        <Input aria-label="Code" mono placeholder="pf1-…" />
      </>,
    );
    const plain = screen.getByLabelText("Plain");
    expect(plain.className).toContain("ui-input");
    expect(plain.className).not.toContain("ui-input-mono");
    expect(screen.getByLabelText("Code").className).toContain("ui-input-mono");
    expect(screen.getByPlaceholderText("pf1-…")).toBe(screen.getByLabelText("Code"));
  });

  it("Select stays a native select with a chevron", () => {
    const { container } = render(
      <Select aria-label="Model" defaultValue="b">
        <option value="a">a</option>
        <option value="b">b</option>
      </Select>,
    );
    const select = screen.getByLabelText("Model") as HTMLSelectElement;
    expect(select.tagName).toBe("SELECT");
    expect(select.value).toBe("b");
    expect(select.className).toContain("ui-select-field");
    expect(container.querySelector("[data-icon='chevron-down']")).not.toBeNull();
  });
});

describe("Chip", () => {
  it.each(["neutral", "violet", "mint", "amber"] as const)("renders the %s tone", (tone) => {
    render(<Chip tone={tone}>{tone}</Chip>);
    expect(screen.getByText(tone).className).toContain(`ui-chip-${tone}`);
  });

  it("is neutral and sans by default, mono on request", () => {
    render(
      <>
        <Chip>plain</Chip>
        <Chip mono>wu</Chip>
      </>,
    );
    expect(screen.getByText("plain").className).toContain("ui-chip-neutral");
    expect(screen.getByText("plain").className).not.toContain("ui-chip-mono");
    expect(screen.getByText("wu").className).toContain("ui-chip-mono");
  });
});

describe("StatePill and StateDot", () => {
  const STATES = ["needs_you", "working", "done", "merged", "idle", "ended"] as const;

  it.each(STATES)("StatePill maps %s to its colour class and shows its own text", (state) => {
    render(<StatePill state={state}>{`text for ${state}`}</StatePill>);
    const pill = screen.getByText(`text for ${state}`);
    expect(pill.className).toContain("ui-pill");
    expect(pill.className).toContain(`ui-pill-${state}`);
    expect(pill.querySelector(`.ui-dot-${state}`)).not.toBeNull();
  });

  it("StatePill passes role and label through", () => {
    render(
      <StatePill state="working" role="group" aria-label="State">
        Working
      </StatePill>,
    );
    expect(screen.getByRole("group", { name: "State" }).textContent).toBe("Working");
  });

  it.each(STATES)("StateDot renders the %s mark, hidden from assistive tech", (state) => {
    const { container } = render(<StateDot state={state} />);
    const dot = container.firstElementChild as HTMLElement;
    expect(dot.className).toContain("ui-dot");
    expect(dot.className).toContain(`ui-dot-${state}`);
    expect(dot.getAttribute("aria-hidden")).toBe("true");
  });
});

describe("CountBadge", () => {
  it("hides at zero", () => {
    const { container } = render(<CountBadge count={0} />);
    expect(container.firstChild).toBeNull();
  });

  it("shows the count with the title", () => {
    render(<CountBadge count={3} title="3 open" />);
    const badge = screen.getByTitle("3 open");
    expect(badge.textContent).toBe("3");
    expect(badge.className).toContain("ui-count");
  });
});

describe("Card and Eyebrow", () => {
  it("Card is the raised card, amber when it needs you", () => {
    render(
      <>
        <Card data-testid="plain">a</Card>
        <Card tone="needs_you" data-testid="amber">
          b
        </Card>
      </>,
    );
    expect(screen.getByTestId("plain").className).toContain("ui-card");
    expect(screen.getByTestId("plain").className).not.toContain("ui-card-needs_you");
    expect(screen.getByTestId("amber").className).toContain("ui-card-needs_you");
  });

  it("Eyebrow renders its text", () => {
    render(<Eyebrow>Paired</Eyebrow>);
    expect(screen.getByText("Paired").className).toContain("eyebrow");
  });
});

describe("Icon", () => {
  it("the name union is the canvas list plus arrow-left, bell and folder", () => {
    expect([...ICON_NAMES].sort()).toEqual([...CANVAS_ICONS, ...APP_ICONS].sort());
  });

  it("every name draws a 24x24 stroke icon in the Lucide idiom", () => {
    for (const name of ICON_NAMES) {
      const { container, unmount } = render(<Icon name={name} />);
      const svg = container.querySelector("svg") as SVGElement;
      expect(svg.getAttribute("viewBox")).toBe("0 0 24 24");
      expect(svg.getAttribute("fill")).toBe("none");
      expect(svg.getAttribute("stroke")).toBe("currentColor");
      expect(svg.getAttribute("stroke-width")).toBe("2");
      expect(svg.getAttribute("stroke-linecap")).toBe("round");
      expect(svg.getAttribute("stroke-linejoin")).toBe("round");
      expect(svg.getAttribute("aria-hidden")).toBe("true");
      expect(svg.getAttribute("data-icon")).toBe(name);
      expect(svg.childElementCount).toBeGreaterThan(0);
      unmount();
    }
  });

  it("is 16 px by default and takes a size", () => {
    const { container } = render(
      <>
        <Icon name="plus" />
        <Icon name="plus" size={13} />
      </>,
    );
    const svgs = container.querySelectorAll("svg");
    expect(svgs[0]?.getAttribute("width")).toBe("16");
    expect(svgs[1]?.getAttribute("width")).toBe("13");
    expect(svgs[1]?.getAttribute("height")).toBe("13");
  });

  it("takes the sizes 11 and 12 that the R1 frames draw (Approach 11)", () => {
    const { container } = render(
      <>
        <Icon name="git-branch" size={11} />
        <Icon name="x" size={12} />
      </>,
    );
    const svgs = container.querySelectorAll("svg");
    expect(svgs[0]?.getAttribute("width")).toBe("11");
    expect(svgs[0]?.getAttribute("height")).toBe("11");
    expect(svgs[1]?.getAttribute("width")).toBe("12");
  });

  it.each(["more", "trash", "square", "minus", "alert"] as const)("draws the new icon %s", (name) => {
    const { container } = render(<Icon name={name} />);
    expect(container.querySelector("svg")?.getAttribute("data-icon")).toBe(name);
    expect(container.querySelector("svg")?.childElementCount).toBeGreaterThan(0);
  });
});

describe("the components are plain", () => {
  const dir = join(__dirname, "..", "ui");
  const files = readdirSync(dir).filter((f) => /\.(ts|tsx)$/.test(f));

  it("the directory holds one file per component and an index", () => {
    for (const name of ["Button", "IconButton", "Input", "Select", "Chip", "StatePill", "StateDot", "CountBadge", "Card", "Eyebrow", "Icon", "Avatar", "NotBuilt"]) {
      expect(files, name).toContain(`${name}.tsx`);
    }
    expect(files).toContain("index.ts");
  });

  it.each(["setTimeout", "setInterval", "requestAnimationFrame", "localStorage", "sessionStorage", "indexedDB", "document.cookie", "console.", "fetch(", "XMLHttpRequest", "WebSocket", "EventSource", "useConnection", "transport"])(
    "no file under src/ui uses %s",
    (word) => {
      for (const file of files) {
        const text = readFileSync(join(dir, file), "utf8");
        expect(text.includes(word), `${file} uses ${word}`).toBe(false);
      }
    },
  );

  it("no screen imports a ui file directly", () => {
    const roots = [join(__dirname, "..", "screens"), join(__dirname, "..", "components")];
    for (const root of roots) {
      for (const file of readdirSync(root)) {
        if (!/\.tsx?$/.test(file)) continue;
        const text = readFileSync(join(root, file), "utf8");
        expect(/from "\.\.\/ui\/[A-Za-z]/.test(text), `${file} imports a ui file directly`).toBe(false);
      }
    }
  });
});
