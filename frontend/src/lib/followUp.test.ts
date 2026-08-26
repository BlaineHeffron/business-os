import { describe, expect, it } from "vitest";
import {
  addBusinessDays,
  addCalendarDays,
  buildFollowUpRequestBody,
  canDraftFollowUpReply,
  defaultDueDate,
  defaultFollowUpTitle,
  dueDateForChip,
  DUE_CHIPS,
  DEFAULT_DUE_CHIP_ID,
  followUpFooterLabels,
  isPastDate,
  isoDate,
  threadStateChip,
} from "./followUp";

// Fixed local-time anchors (month is 0-based). Constructed without a TZ
// suffix so they read in the test runner's local day, matching how the
// helpers read the operator's local day.
const WED = new Date(2026, 5, 24); // Wed 2026-06-24
const FRI = new Date(2026, 5, 26); // Fri 2026-06-26
const SAT = new Date(2026, 5, 27); // Sat 2026-06-27

describe("isoDate", () => {
  it("formats local Y-M-D with zero padding", () => {
    expect(isoDate(new Date(2026, 0, 5))).toBe("2026-01-05");
    expect(isoDate(WED)).toBe("2026-06-24");
  });
});

describe("addBusinessDays", () => {
  it("skips the weekend: Wed + 3 biz = next Mon", () => {
    // Thu, Fri, Mon → 2026-06-29
    expect(addBusinessDays(WED, 3)).toBe("2026-06-29");
  });

  it("Fri + 1 biz = Mon", () => {
    expect(addBusinessDays(FRI, 1)).toBe("2026-06-29");
  });

  it("Fri + 2 biz = Tue", () => {
    expect(addBusinessDays(FRI, 2)).toBe("2026-06-30");
  });

  it("counts forward from a weekend start without counting the weekend", () => {
    // Sat + 1 biz = Mon 2026-06-29
    expect(addBusinessDays(SAT, 1)).toBe("2026-06-29");
  });

  it("n=0 returns the base date unchanged", () => {
    expect(addBusinessDays(WED, 0)).toBe("2026-06-24");
  });
});

describe("addCalendarDays", () => {
  it("1 week = +7 calendar days, weekend included", () => {
    expect(addCalendarDays(WED, 7)).toBe("2026-07-01");
  });
});

describe("DUE_CHIPS / defaults", () => {
  it("default chip id resolves to a chip", () => {
    expect(DUE_CHIPS.some((c) => c.id === DEFAULT_DUE_CHIP_ID)).toBe(true);
  });

  it("offers exactly 2d / 3d / 1w presets", () => {
    expect(DUE_CHIPS.map((c) => c.id)).toEqual(["2d", "3d", "1w"]);
  });

  it("defaultDueDate = today + 3 business days", () => {
    expect(defaultDueDate(WED)).toBe(addBusinessDays(WED, 3));
    expect(defaultDueDate(WED)).toBe("2026-06-29");
  });

  it("dueDateForChip matches the chip mode", () => {
    const week = DUE_CHIPS.find((c) => c.id === "1w")!;
    const twoBiz = DUE_CHIPS.find((c) => c.id === "2d")!;
    expect(dueDateForChip(week, WED)).toBe(addCalendarDays(WED, 7));
    expect(dueDateForChip(twoBiz, WED)).toBe(addBusinessDays(WED, 2));
  });
});

describe("isPastDate", () => {
  it("rejects a strictly earlier ISO date", () => {
    expect(isPastDate("2026-06-23", "2026-06-24")).toBe(true);
  });
  it("accepts today and future", () => {
    expect(isPastDate("2026-06-24", "2026-06-24")).toBe(false);
    expect(isPastDate("2026-06-25", "2026-06-24")).toBe(false);
  });
  it("treats empty as not-past (nothing chosen yet)", () => {
    expect(isPastDate("", "2026-06-24")).toBe(false);
  });
});

describe("defaultFollowUpTitle", () => {
  it("prefixes the subject", () => {
    expect(defaultFollowUpTitle("Quote for repaint")).toBe(
      "Follow up: Quote for repaint",
    );
  });
  it("falls back when subject is empty/missing", () => {
    expect(defaultFollowUpTitle("")).toBe("Follow up");
    expect(defaultFollowUpTitle(null)).toBe("Follow up");
    expect(defaultFollowUpTitle(undefined)).toBe("Follow up");
  });
});

describe("threadStateChip", () => {
  it("maps each stored state to the right label + tone", () => {
    expect(threadStateChip("draft_created")).toEqual({
      label: "Draft created",
      tone: "neutral",
    });
    expect(threadStateChip("sent_waiting_reply")).toEqual({
      label: "Waiting on reply",
      tone: "info",
    });
    expect(threadStateChip("replied_after_send")).toEqual({
      label: "They replied",
      tone: "ok",
    });
    expect(threadStateChip("stale_unknown")).toEqual({
      label: "Can't check",
      tone: "neutral",
    });
  });

  it("renders no chip for not_applicable / unknown / absent", () => {
    expect(threadStateChip("not_applicable")).toBeNull();
    expect(threadStateChip("something_else")).toBeNull();
    expect(threadStateChip(null)).toBeNull();
    expect(threadStateChip(undefined)).toBeNull();
  });

  it("never maps a thread state to critical (red is reserved)", () => {
    for (const s of [
      "draft_created",
      "sent_waiting_reply",
      "replied_after_send",
      "stale_unknown",
    ]) {
      expect(threadStateChip(s)?.tone).not.toBe("critical");
    }
  });
});

describe("followUpFooterLabels", () => {
  it("keeps the existing labels when OFF", () => {
    expect(followUpFooterLabels(false)).toEqual({
      approve: "Approve → Gmail draft",
      approveDirty: "Save & approve → Gmail draft",
    });
  });
  it("calls out the follow-up when ON", () => {
    expect(followUpFooterLabels(true)).toEqual({
      approve: "Approve → Gmail draft + follow-up",
      approveDirty: "Save & approve → Gmail draft + follow-up",
    });
  });
});

describe("buildFollowUpRequestBody", () => {
  const base = {
    enabled: true,
    valid: true,
    dueDate: "2026-06-29",
    title: "Follow up: Quote",
    note: "ping them",
  };

  it("returns undefined when disabled (payload omitted)", () => {
    expect(buildFollowUpRequestBody({ ...base, enabled: false })).toBeUndefined();
  });

  it("returns undefined when enabled but invalid (e.g. bad custom date)", () => {
    expect(buildFollowUpRequestBody({ ...base, valid: false })).toBeUndefined();
  });

  it("builds the wire body when enabled + valid, create_follow_up_draft always false", () => {
    expect(buildFollowUpRequestBody(base)).toEqual({
      enabled: true,
      due_date: "2026-06-29",
      title: "Follow up: Quote",
      context: "ping them",
      create_follow_up_draft: false,
    });
  });
});

describe("canDraftFollowUpReply", () => {
  it("only when waiting on reply AND due/overdue", () => {
    expect(
      canDraftFollowUpReply({ threadState: "sent_waiting_reply", dueLane: "overdue" }),
    ).toBe(true);
    expect(
      canDraftFollowUpReply({ threadState: "sent_waiting_reply", dueLane: "due_today" }),
    ).toBe(true);
  });

  it("not when upcoming, or wrong thread state", () => {
    expect(
      canDraftFollowUpReply({ threadState: "sent_waiting_reply", dueLane: "upcoming" }),
    ).toBe(false);
    expect(
      canDraftFollowUpReply({ threadState: "draft_created", dueLane: "overdue" }),
    ).toBe(false);
    expect(
      canDraftFollowUpReply({ threadState: "replied_after_send", dueLane: "overdue" }),
    ).toBe(false);
    expect(canDraftFollowUpReply({ threadState: null, dueLane: null })).toBe(false);
  });
});
