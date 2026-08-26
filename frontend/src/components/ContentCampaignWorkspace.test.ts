import { describe, expect, it } from "vitest";
import {
  browserTimeZoneLabel,
  campaignRecoveryGuidance,
  campaignPlanLocalFormReducer,
  initialCampaignPlanLocalFormState,
} from "./ContentCampaignWorkspace";

describe("ContentCampaignWorkspace plan-local state", () => {
  it("clears URL, channel, date, and launch choices when the plan changes", () => {
    const initial = initialCampaignPlanLocalFormState("plan-a", "2026-08-13");
    const dirty = campaignPlanLocalFormReducer(initial, {
      type: "patch",
      patch: {
        selectedChannelIds: ["buffer-a"],
        expectedUrl: "https://example.com/plan-a",
        publishedAt: "2026-08-20",
        launchMode: "schedule",
      },
    });

    const switched = campaignPlanLocalFormReducer(dirty, {
      type: "plan_changed",
      planItemId: "plan-b",
      today: "2026-08-14",
    });

    expect(switched).toEqual(
      initialCampaignPlanLocalFormState("plan-b", "2026-08-14"),
    );
  });

  it("clears an auto-filled canonical URL when social state is removed", () => {
    const loaded = campaignPlanLocalFormReducer(
      initialCampaignPlanLocalFormState("plan-a", "2026-08-13"),
      {
        type: "social_loaded",
        channelIds: ["buffer-a"],
        canonicalUrl: "https://example.com/plan-a",
      },
    );

    expect(
      campaignPlanLocalFormReducer(loaded, { type: "social_cleared" }),
    ).toEqual({
      ...loaded,
      selectedChannelIds: [],
      expectedUrl: "",
    });
  });

  it("gives distinct safe recovery guidance for canonical drift and uncertain delivery", () => {
    expect(
      campaignRecoveryGuidance("requires_review", "canonical_url_mismatch", []),
    ).toContain("No social posts were created");

    expect(
      campaignRecoveryGuidance("requires_review", "delivery_outcome_unknown", [
        {
          job_id: "job-1",
          status: "delivery_outcome_unknown",
          attempts: 1,
          last_error: "connection closed",
          dry_run: false,
          provider_object_id: null,
        },
      ]),
    ).toContain("will not retry");

    expect(campaignRecoveryGuidance("blog_dry_run", null, [])).toContain(
      "no live blog post and no social posts were created",
    );
  });

  it("labels browser-local scheduling with a timezone when available", () => {
    expect(browserTimeZoneLabel()).toMatch(/local time|Your time ·/);
  });
});
