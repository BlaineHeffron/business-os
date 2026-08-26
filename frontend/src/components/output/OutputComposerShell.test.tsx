import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import OutputComposer, { canCloseComposer } from "../OutputComposer";
import OutputComposerShell from "./OutputComposerShell";

describe("OutputComposerShell", () => {
  it("renders one accessible workspace with context, typed tabs, and footer", () => {
    const markup = renderToStaticMarkup(
      <OutputComposerShell
        title="Create output"
        mode="blank"
        tabs={[
          { id: "email_draft_reply", label: "Email" },
          { id: "follow_up_task", label: "Follow-up task" },
        ]}
        activeTab="email_draft_reply"
        onSelectTab={() => undefined}
        contextTitle="Governed context"
        context={<p>Saved operator context</p>}
        footer={<button>Stage draft</button>}
        onClose={() => undefined}
      >
        <label>
          Subject
          <input name="subject" />
        </label>
      </OutputComposerShell>,
    );

    expect(markup).toContain('role="dialog"');
    expect(markup).toContain('aria-modal="true"');
    expect(markup).toContain('role="tablist"');
    expect(markup).toContain('aria-selected="true"');
    expect(markup).toContain("Saved operator context");
    expect(markup).toContain("Stage draft");
    expect(markup).toContain('name="subject"');
  });

  it("renders blank email fields as operator-owned with a clear destination", () => {
    const markup = renderToStaticMarkup(
      <OutputComposer
        availableKinds={["email_draft_reply", "follow_up_task"]}
        onClose={() => undefined}
        onCreated={() => undefined}
        onUnauthorized={() => undefined}
      />,
    );

    expect(markup).toContain("Gmail draft");
    expect(markup).toContain("BusinessOS never sends it");
    expect(markup).toContain("Write manually");
    expect(markup).not.toContain("inferred");
  });

  it("guards authored content only until its work item exists", () => {
    const refuseDiscard = vi.fn(() => false);
    expect(canCloseComposer(["", "  ", "\n"], false, refuseDiscard)).toBe(true);
    expect(refuseDiscard).not.toHaveBeenCalled();

    expect(canCloseComposer(["", "Draft body", ""], false, refuseDiscard)).toBe(
      false,
    );
    expect(refuseDiscard).toHaveBeenCalledOnce();

    expect(canCloseComposer(["Draft body"], true, refuseDiscard)).toBe(true);
    expect(refuseDiscard).toHaveBeenCalledOnce();
    expect(canCloseComposer(["Draft body"], false, () => true)).toBe(true);
  });
});
