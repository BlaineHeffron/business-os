import { describe, expect, it } from "vitest";
import type { SocialPostProposalWithRevision } from "../types/generated/SocialPostProposalWithRevision";
import {
  nextSocialProposalId,
  targetReadyForProvider,
  targetRequest,
} from "./SocialPublishing";

function proposal(id: string): SocialPostProposalWithRevision {
  return {
    proposal: {
      proposal_id: id,
      source_id: null,
      source_content_draft_id: null,
      source_content_draft_revision: null,
      canonical_url: `https://example.com/${id}`,
      status: "staged",
      targets: [],
      approved_by: null,
      approved_revision: null,
      created_at_ms: 1,
      updated_at_ms: 1,
    },
    revision: 1,
  };
}

describe("social proposal keyboard navigation", () => {
  const proposals = [proposal("proposal-a"), proposal("proposal-b")];

  it("moves with arrows or j/k and wraps", () => {
    expect(nextSocialProposalId(proposals, "proposal-a", "ArrowDown")).toBe("proposal-b");
    expect(nextSocialProposalId(proposals, "proposal-b", "j")).toBe("proposal-a");
    expect(nextSocialProposalId(proposals, "proposal-a", "k")).toBe("proposal-b");
  });

  it("compares only editable wire fields from a stored target", () => {
    const editable = {
      channel_id: "channel-a",
      text: "Post text",
      image_url: null,
      utm: { source: "linkedin", medium: "social", campaign: "launch", content: null },
      schedule_mode: "queue" as const,
      due_at: null,
    };
    const storedRuntimeShape = {
      ...editable,
      target_id: "target-a",
      tracked_url: "https://example.com/post?utm_source=linkedin",
      outbox_job: null,
    };

    expect(targetRequest(storedRuntimeShape)).toEqual(editable);
  });

  it("requires media only for the supported Instagram feed shape", () => {
    const editable = {
      channel_id: "channel-a",
      text: "Post text",
      image_url: null,
      utm: {},
      schedule_mode: "queue" as const,
      due_at: null,
    };

    expect(targetReadyForProvider(editable, "instagram")).toBe(false);
    expect(targetReadyForProvider(editable, "Instagram")).toBe(false);
    expect(targetReadyForProvider(editable, "facebook")).toBe(true);
    expect(targetReadyForProvider(editable, "linkedin")).toBe(true);
    expect(targetReadyForProvider(editable, "googlebusiness")).toBe(true);
    expect(
      targetReadyForProvider(
        { ...editable, image_url: "https://example.com/stay.jpg" },
        "instagram",
      ),
    ).toBe(true);
  });
});
