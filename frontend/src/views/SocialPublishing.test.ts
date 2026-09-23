import { describe, expect, it } from "vitest";
import type { SocialPostProposalWithRevision } from "../types/generated/SocialPostProposalWithRevision";
import type { SocialPublishedSource } from "../types/generated/SocialPublishedSource";
import type { SocialPublishingChannel } from "../types/generated/SocialPublishingChannel";
import {
  approveBlockedReason,
  decisionNotice,
  nextSocialProposalId,
  proposalListLabel,
  scheduleModeLabel,
  targetInput,
  targetReadyForProvider,
  targetRequest,
  targetsForDestinationChange,
  trackedUrlLabel,
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

describe("proposal list labels", () => {
  it("uses the source title, otherwise the URL path, otherwise ad-hoc", () => {
    expect(proposalListLabel({ canonical_url: null, source_id: null }, [])).toBe(
      "Ad-hoc post",
    );
    expect(
      proposalListLabel(
        { canonical_url: "https://example.com/blog/post", source_id: null },
        [],
      ),
    ).toBe("/blog/post");
    expect(
      proposalListLabel(
        { canonical_url: null, source_id: "src-1" },
        [{ source_id: "src-1", title: "Closed Christmas Day" } as never],
      ),
    ).toBe("Closed Christmas Day");
  });
});

describe("url-less target UTM", () => {
  const channel: SocialPublishingChannel = {
    channel_id: "channel-a",
    name: "Company LinkedIn",
    platform: "linkedin",
  };
  const emptyUtm = {
    source: null,
    medium: null,
    campaign: null,
    content: null,
  };
  const source = (canonical_url: string | null): SocialPublishedSource =>
    ({
      source_id: "src-1",
      source_kind: "adhoc",
      external_id: "adhoc-1",
      title: "Closed Christmas Day",
      canonical_url,
      generation_status: "ready",
      revision: 1,
    }) as SocialPublishedSource;

  it("does not seed UTM without a destination",
    () => {
      expect(targetInput(channel).utm).toEqual(emptyUtm);
      expect(targetInput(channel, source(null)).utm).toEqual(emptyUtm);
    },
  );

  it("seeds UTM when the source has a destination",
    () => {
      expect(
        targetInput(channel, source("https://example.com/holiday-hours")).utm,
      ).toEqual({
        source: "linkedin",
        medium: "social",
        campaign: "blog",
        content: null,
      });
    },
  );

  it("strips UTM in targetRequest when there is no destination",
    () => {
      const withUtm = {
        channel_id: "channel-a",
        text: "Closed December 25.",
        image_url: null,
        utm: {
          source: "linkedin",
          medium: "social",
          campaign: "blog",
          content: null,
        },
        schedule_mode: "queue" as const,
        due_at: null,
      };
      expect(targetRequest(withUtm, false).utm).toEqual(emptyUtm);
      expect(targetRequest(withUtm, true).utm).toEqual(withUtm.utm);
    },
  );

  it("clears UTM when the destination is removed and seeds it when one is added", () => {
    const withUtm = targetInput(channel, source("https://example.com/hours"));
    const withoutUtm = targetInput(channel, source(null));
    expect(
      targetsForDestinationChange([withUtm], [channel], true, false)[0].utm,
    ).toEqual(emptyUtm);
    expect(
      targetsForDestinationChange([withoutUtm], [channel], false, true)[0].utm,
    ).toEqual({
      source: "linkedin",
      medium: "social",
      campaign: "blog",
      content: null,
    });
    expect(
      targetsForDestinationChange([withUtm], [channel], true, true),
    ).toEqual([withUtm]);
  });

  it("labels a missing tracked URL as none", () => {
    expect(trackedUrlLabel("")).toBe("None");
    expect(trackedUrlLabel("https://example.com/post")).toBe(
      "https://example.com/post",
    );
  });
});

describe("decisionNotice", () => {
  it("describes each decision, including live versus dry-run approval", () => {
    expect(decisionNotice("approve", true)).toMatch(/queued independently/);
    expect(decisionNotice("approve", false)).toMatch(/dry-run/);
    expect(decisionNotice("reject", true)).toBe("Proposal rejected.");
    expect(decisionNotice("redraft", false)).toMatch(/re-drafting under the current Buffer channels/);
  });
});

describe("approveBlockedReason", () => {
  it("explains why approval is disabled, unsaved edits first", () => {
    expect(approveBlockedReason(true, false)).toBe("Save changes before approval");
    expect(approveBlockedReason(false, false)).toBe("Instagram needs a public image before approval");
    expect(approveBlockedReason(false, true)).toBeUndefined();
  });
});

describe("scheduleModeLabel", () => {
  it("labels queue, draft, and scheduled modes", () => {
    expect(scheduleModeLabel("queue", null, "Your time · UTC")).toBe(
      "Next Buffer queue slot",
    );
    expect(scheduleModeLabel("draft", "2026-08-20T14:00:00Z", "Your time · UTC")).toBe(
      "Buffer draft (not scheduled)",
    );
    expect(
      scheduleModeLabel("scheduled", "2026-08-20T14:00:00Z", "Your time · UTC"),
    ).toMatch(/Your time · UTC/);
  });
});
