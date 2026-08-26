import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { EmailTriageRule } from "../types/generated/EmailTriageRule";
import RuleEditor from "./RuleEditor";

const noop = () => {};

function renderEditor(seed: EmailTriageRule | null = null): string {
  return renderToStaticMarkup(
    <RuleEditor
      editing={null}
      seed={seed}
      onSaved={noop}
      onCancel={noop}
      onUnauthorized={noop}
      onConflict={noop}
      onDraftChange={noop}
      onTestDraft={noop}
      aiTriageEnabled={false}
    />,
  );
}

describe("new rule category choice", () => {
  it("starts without a selected category", () => {
    const html = renderEditor();

    expect(html).toContain(
      '<option value="" selected="">Choose an existing category…</option>',
    );
    expect(html).toContain("Create new category for this rule");
    expect(html).not.toContain("Create work items for this category");
  });

  it("does not inherit the source message category", () => {
    const html = renderEditor({
      rule_id: "from-example",
      priority: 100,
      match_mode: "all",
      pinned_category: "inbound_email",
      enabled: true,
      conditions: [
        {
          field: "from",
          op: "contains",
          value: "sender@example.com",
          header_name: null,
        },
      ],
      conditions_v2: [],
    });

    expect(html).toContain(
      '<option value="" selected="">Choose an existing category…</option>',
    );
  });
});
