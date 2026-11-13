// AC-1: the Markdown renderer of the chat's replies (Approach 2). It builds React elements from text
// nodes only: no HTML pass-through, no `dangerouslySetInnerHTML` and no live link.
import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Markdown } from "../components/markdown";

function html(text: string): HTMLElement {
  const { container } = render(<Markdown text={text} />);
  return container;
}

describe("the constructs", () => {
  it("renders paragraphs, split at a blank line, and keeps a line break inside one", () => {
    const c = html("First line\nsecond line\n\nNext paragraph");
    const paragraphs = c.querySelectorAll("p");
    expect(paragraphs).toHaveLength(2);
    expect(paragraphs[0]?.textContent).toBe("First line\nsecond line");
    expect(paragraphs[1]?.textContent).toBe("Next paragraph");
  });

  it("renders **bold** and *italic*", () => {
    const c = html("a **strong** and an *emphasis* word");
    expect(c.querySelector("strong")?.textContent).toBe("strong");
    expect(c.querySelector("em")?.textContent).toBe("emphasis");
    expect(c.querySelector("p")?.textContent).toBe("a strong and an emphasis word");
  });

  it("renders inline code as a code chip, and leaves the marks inside it alone", () => {
    const c = html("Run `cargo test **now**` first");
    const code = c.querySelector("code");
    expect(code?.textContent).toBe("cargo test **now**");
    expect(c.querySelector("strong")).toBeNull();
  });

  it("renders a fenced code block as mono text that keeps its lines and its marks", () => {
    const c = html("Before\n\n```rust\nfn main() {\n  let a = **b**;\n}\n```\n\nAfter");
    const pre = c.querySelector("pre");
    expect(pre?.textContent).toBe("fn main() {\n  let a = **b**;\n}");
    expect(pre?.querySelector("code")).not.toBeNull();
    expect(c.querySelectorAll("p")).toHaveLength(2);
    expect(c.querySelector("strong")).toBeNull();
  });

  it("closes a fence that never ends at the end of the text", () => {
    const c = html("```\nopen fence\nstill code");
    expect(c.querySelector("pre")?.textContent).toBe("open fence\nstill code");
  });

  it("renders `-` lists and `1.` lists", () => {
    const c = html("- one\n- two **b**\n\n1. first\n2. second");
    const bullets = c.querySelectorAll("ul > li");
    expect(Array.from(bullets).map((li) => li.textContent)).toEqual(["one", "two b"]);
    expect(bullets[1]?.querySelector("strong")?.textContent).toBe("b");
    const numbered = c.querySelectorAll("ol > li");
    expect(Array.from(numbered).map((li) => li.textContent)).toEqual(["first", "second"]);
  });

  it("renders a heading as a bold line", () => {
    const c = html("## The plan\nBody text");
    const heading = c.querySelector(".md-heading");
    expect(heading?.textContent).toBe("The plan");
    expect(heading?.querySelector("strong")?.textContent).toBe("The plan");
    expect(c.querySelector("h1, h2, h3, h4, h5, h6")).toBeNull();
  });

  it("shows a link as its text with the URL in the title, never as an anchor", () => {
    const c = html("See [the docs](https://example.com/docs) now");
    expect(c.querySelector("a")).toBeNull();
    const link = c.querySelector("[title='https://example.com/docs']");
    expect(link?.textContent).toBe("the docs");
  });

  it("renders text with no mark as it is", () => {
    const c = html("2 * 3 = 6 and a_b_c");
    expect(c.textContent).toBe("2 * 3 = 6 and a_b_c");
    expect(c.querySelector("em, strong")).toBeNull();
  });
});

describe("no HTML", () => {
  it("shows <script>, <img onerror> and <b> as literal text with no element of that kind", () => {
    const text = '<script>window.__owned = 1</script>\n\n<img src=x onerror="window.__owned = 2">\n\n<b>bold</b>';
    const c = html(text);
    expect(c.querySelector("script, img, b")).toBeNull();
    expect(c.textContent).toContain("<script>window.__owned = 1</script>");
    expect(c.textContent).toContain('<img src=x onerror="window.__owned = 2">');
    expect(c.textContent).toContain("<b>bold</b>");
    expect((window as unknown as { __owned?: number }).__owned).toBeUndefined();
  });

  it("shows a javascript: link, in HTML or Markdown form, as text with no anchor", () => {
    const c = html('<a href="javascript:alert(1)">x</a> and [y](javascript:alert(2))');
    expect(c.querySelector("a")).toBeNull();
    expect(c.textContent).toContain('<a href="javascript:alert(1)">x</a>');
    // The Markdown form shows its text; the URL is only a title.
    expect(c.querySelector("[title='javascript:alert(2)']")?.textContent).toBe("y");
    expect(c.querySelector("[href]")).toBeNull();
  });

  it("sets no markup inside a code block or a heading either", () => {
    const c = html("# <i>head</i>\n\n```\n<u>code</u>\n```");
    expect(c.querySelector("i, u")).toBeNull();
    expect(c.textContent).toContain("<i>head</i>");
    expect(c.textContent).toContain("<u>code</u>");
  });
});
