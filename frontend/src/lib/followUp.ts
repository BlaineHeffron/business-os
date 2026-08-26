import type { StatusTone } from "./status";

/**
 * Outbound email follow-up workflow (issue #185) — pure, framework-free
 * presentation + date logic, so it unit-tests without a DOM.
 *
 * Two responsibilities live here:
 *  1. Compute the explicit follow-up due date the panel sends to the backend.
 *     The default window (3 BUSINESS days) is owned by the frontend per the
 *     locked product decision (plan §2): the UI resolves it to an ISO
 *     YYYY-MM-DD string and the backend only validates the date it receives.
 *  2. Map the backend's machine thread-state names (plan §3) to warm operator
 *     labels + a tone. Red is reserved for overdue-task escalation, so no
 *     thread state ever maps to "critical".
 */

/** ISO YYYY-MM-DD for a Date, read in the operator's local day. */
export function isoDate(d: Date): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

/** A fresh local-midnight copy, so date math never drifts on DST/time. */
function startOfDay(base: Date): Date {
  return new Date(base.getFullYear(), base.getMonth(), base.getDate());
}

/**
 * Add `n` business days (skipping Sat/Sun) to `base`, returning ISO
 * YYYY-MM-DD. n=0 returns `base`'s date unchanged.
 */
export function addBusinessDays(base: Date, n: number): string {
  const d = startOfDay(base);
  let added = 0;
  while (added < n) {
    d.setDate(d.getDate() + 1);
    const day = d.getDay();
    if (day !== 0 && day !== 6) added += 1;
  }
  return isoDate(d);
}

/** Add `n` calendar days to `base`, returning ISO YYYY-MM-DD. */
export function addCalendarDays(base: Date, n: number): string {
  const d = startOfDay(base);
  d.setDate(d.getDate() + n);
  return isoDate(d);
}

/** Preset due-date chips offered in the follow-up section (plan §5.1). */
export type DueChipMode = "business" | "calendar";
export interface DueChip {
  id: string;
  /** Short chip face. */
  label: string;
  /** Longer, screen-reader-friendly description / title. */
  description: string;
  days: number;
  mode: DueChipMode;
}

export const DUE_CHIPS: readonly DueChip[] = [
  { id: "2d", label: "2 business days", description: "2 business days from today", days: 2, mode: "business" },
  { id: "3d", label: "3 business days", description: "3 business days from today (default)", days: 3, mode: "business" },
  { id: "1w", label: "1 week", description: "1 week (7 days) from today", days: 7, mode: "calendar" },
];

/** The locked default (plan §2): today + 3 business days. */
export const DEFAULT_DUE_CHIP_ID = "3d";

/** Resolve a preset chip to an explicit ISO due date relative to `base`. */
export function dueDateForChip(chip: DueChip, base: Date): string {
  return chip.mode === "business"
    ? addBusinessDays(base, chip.days)
    : addCalendarDays(base, chip.days);
}

/** ISO due date for the default chip relative to `base` (today + 3 biz days). */
export function defaultDueDate(base: Date): string {
  const chip = DUE_CHIPS.find((c) => c.id === DEFAULT_DUE_CHIP_ID);
  // DEFAULT_DUE_CHIP_ID always matches a chip; fall back defensively.
  return chip ? dueDateForChip(chip, base) : addBusinessDays(base, 3);
}

/**
 * True if `iso` (YYYY-MM-DD) is strictly before `today` (YYYY-MM-DD). ISO
 * dates compare correctly as plain strings, so the custom picker can reject
 * past dates without parsing.
 */
export function isPastDate(iso: string, today: string): boolean {
  return iso.length > 0 && iso < today;
}

/** Default editable title for a new follow-up: "Follow up: <subject>". */
export function defaultFollowUpTitle(subject: string | null | undefined): string {
  const s = (subject ?? "").trim();
  return s ? `Follow up: ${s}` : "Follow up";
}

/**
 * Warm label + tone for a stored Gmail thread state (plan §3). Returns null
 * for `not_applicable` (and any unknown/absent state) so callers render no
 * chip at all. Never returns "critical" — red is reserved for overdue
 * escalation, thread state is never alarming.
 */
export interface ThreadStateChip {
  label: string;
  tone: StatusTone;
}

const THREAD_STATE_CHIPS: Record<string, ThreadStateChip> = {
  // Gmail draft approved; no sent outbound observed yet — neutral.
  draft_created: { label: "Draft created", tone: "neutral" },
  // A sent outbound exists after approval; awaiting reply — active/blue.
  sent_waiting_reply: { label: "Waiting on reply", tone: "info" },
  // Inbound after the sent anchor → auto-resolved — success/green.
  replied_after_send: { label: "They replied", tone: "ok" },
  // Thread unreadable / no credential / ambiguous — muted grey.
  stale_unknown: { label: "Can't check", tone: "neutral" },
  // not_applicable: intentionally absent → no chip.
};

export function threadStateChip(
  state: string | null | undefined,
): ThreadStateChip | null {
  if (!state) return null;
  return THREAD_STATE_CHIPS[state] ?? null;
}

/**
 * The follow-up section's resolved decision, bubbled up to the panel so it can
 * relabel the footer and assemble the approve payload. `valid` is false only
 * when the section is enabled but the (custom) due date is missing/past.
 */
export interface FollowUpDecision {
  enabled: boolean;
  valid: boolean;
  /** Explicit ISO YYYY-MM-DD the backend will validate + store. */
  dueDate: string;
  title: string;
  note: string;
}

/**
 * Approve-footer labels (plan §5.1). When the follow-up is ON the primary
 * button calls out the extra effect; OFF keeps the existing labels verbatim so
 * the unchanged flow reads identically.
 */
export function followUpFooterLabels(enabled: boolean): {
  approve: string;
  approveDirty: string;
} {
  return enabled
    ? {
        approve: "Approve → Gmail draft + follow-up",
        approveDirty: "Save & approve → Gmail draft + follow-up",
      }
    : {
        approve: "Approve → Gmail draft",
        approveDirty: "Save & approve → Gmail draft",
      };
}

/**
 * Assemble the `follow_up` body for the approve request, or `undefined` when
 * the section is off / incomplete so the existing approve payload is untouched.
 * Field names are the wire contract (plan §4.3); `create_follow_up_draft` is
 * always false in v1 (auto pre-draft is v1.1).
 */
export function buildFollowUpRequestBody(decision: FollowUpDecision) {
  if (!decision.enabled || !decision.valid) return undefined;
  return {
    enabled: true,
    due_date: decision.dueDate,
    title: decision.title,
    context: decision.note,
    create_follow_up_draft: false,
  };
}

/**
 * Whether a follow-up task should show the explicit "Draft follow-up reply"
 * action (plan §5.2): only when a send was observed and we're still waiting,
 * and the task itself is due or overdue. v1 is an explicit operator action.
 */
export function canDraftFollowUpReply(args: {
  threadState: string | null | undefined;
  /** From the task escalation lane: "overdue" | "due_today" | ... */
  dueLane: string | null | undefined;
}): boolean {
  return (
    args.threadState === "sent_waiting_reply" &&
    (args.dueLane === "overdue" || args.dueLane === "due_today")
  );
}
