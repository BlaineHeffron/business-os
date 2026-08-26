import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { Surface } from "./Card";

describe("Surface", () => {
  it("can provide a semantic heading and always permits grid children to shrink", () => {
    const markup = renderToStaticMarkup(
      <Surface accent="sky" title="Delivery" titleAs="h2">
        <p>Per-channel status</p>
      </Surface>,
    );

    expect(markup).toContain("<h2");
    expect(markup).toContain("Delivery</h2>");
    expect(markup).toContain("min-w-0");
  });
});
