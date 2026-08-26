import { describe, expect, it } from "vitest";
import type { ContentPlanItemWithRevision } from "../types/generated/ContentPlanItemWithRevision";
import { nextContentPlanId } from "./ContentPlans";

function entry(id: string): ContentPlanItemWithRevision {
  return {
    item: {
      plan_item_id: id,
      status: "planned",
      topic: id,
      angle: null,
      format: null,
      target_query: null,
      audience: null,
      notes: null,
      work_item_id: null,
      published_url: null,
      collision_summary: null,
      created_at_ms: 1,
      updated_at_ms: 1,
    },
    revision: 1,
    draft_state: "none",
    active_draft_id: null,
  };
}

describe("content plan keyboard navigation", () => {
  const entries = [entry("plan-a"), entry("plan-b"), entry("plan-c")];

  it("moves with arrows or j/k and wraps at the ends", () => {
    expect(nextContentPlanId(entries, "plan-a", "ArrowDown")).toBe("plan-b");
    expect(nextContentPlanId(entries, "plan-b", "k")).toBe("plan-a");
    expect(nextContentPlanId(entries, "plan-c", "j")).toBe("plan-a");
    expect(nextContentPlanId(entries, "plan-a", "ArrowUp")).toBe("plan-c");
  });

  it("supports list boundary keys", () => {
    expect(nextContentPlanId(entries, "plan-b", "Home")).toBe("plan-a");
    expect(nextContentPlanId(entries, "plan-b", "End")).toBe("plan-c");
  });
});
