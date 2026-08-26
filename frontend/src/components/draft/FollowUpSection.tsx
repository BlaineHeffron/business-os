import { useEffect, useState } from "react";
import {
  DEFAULT_DUE_CHIP_ID,
  DUE_CHIPS,
  defaultFollowUpTitle,
  dueDateForChip,
  isPastDate,
  isoDate,
  type FollowUpDecision,
} from "../../lib/followUp";

/**
 * Collapsible "Follow up if no reply" section shown above the approve footer
 * while an email reply draft is staged (plan §5.1). Default collapsed + OFF so
 * the existing approve flow is unchanged when unused.
 *
 * Owns its own inputs and bubbles a resolved {@link FollowUpDecision} up via
 * `onChange`; the panel uses that to relabel the footer and assemble the
 * approve payload. The default due date (today + 3 business days) is computed
 * client-side to an explicit ISO date — the backend only validates it.
 *
 * Remount per draft (key on draft id) so the subject-derived title default and
 * chip selection reset for each draft.
 */
export default function FollowUpSection({
  subject,
  disabled,
  onChange,
}: {
  subject: string;
  /** Disable all inputs while an action is in flight. */
  disabled: boolean;
  onChange: (decision: FollowUpDecision) => void;
}) {
  const [open, setOpen] = useState(false);
  const [enabled, setEnabled] = useState(false);
  const [chipId, setChipId] = useState<string>(DEFAULT_DUE_CHIP_ID);
  const [customDate, setCustomDate] = useState("");
  const [title, setTitle] = useState(() => defaultFollowUpTitle(subject));
  const [note, setNote] = useState("");

  const today = isoDate(new Date());
  const isCustom = chipId === "custom";
  const presetChip = DUE_CHIPS.find((c) => c.id === chipId);
  const dueDate = isCustom
    ? customDate
    : presetChip
      ? dueDateForChip(presetChip, new Date())
      : "";

  const dueError =
    enabled && isCustom
      ? !customDate
        ? "Pick a date."
        : isPastDate(customDate, today)
          ? "Choose today or a later date."
          : null
      : null;

  const effectiveTitle = title.trim() || defaultFollowUpTitle(subject);
  const valid = enabled ? Boolean(dueDate) && !dueError : true;

  // Bubble the resolved decision whenever any input changes.
  useEffect(() => {
    onChange({ enabled, valid, dueDate, title: effectiveTitle, note });
  }, [onChange, enabled, valid, dueDate, effectiveTitle, note]);

  return (
    <div className="rounded-lg border border-zinc-800 bg-zinc-900/40">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center gap-2 px-3 py-2 text-left text-xs font-medium text-zinc-300 hover:text-zinc-100"
      >
        <span
          aria-hidden
          className={`inline-block transition-transform ${open ? "rotate-90" : ""}`}
        >
          ▸
        </span>
        <span>Follow up if no reply</span>
        {enabled ? (
          <span className="ml-1 rounded-full bg-sky-500/10 px-2 py-0.5 text-[11px] font-normal text-sky-300 ring-1 ring-inset ring-sky-500/30">
            on · {dueDate || "no date"}
          </span>
        ) : (
          <span className="ml-1 text-[11px] font-normal text-zinc-500">
            optional
          </span>
        )}
      </button>

      {open ? (
        <div className="flex flex-col gap-3 border-t border-zinc-800 px-3 py-3 text-xs">
          <label className="flex items-center gap-2 text-zinc-200">
            <input
              type="checkbox"
              checked={enabled}
              disabled={disabled}
              onChange={(e) => setEnabled(e.target.checked)}
              className="h-3.5 w-3.5 accent-sky-500"
            />
            <span>Remind me to follow up if I don&rsquo;t hear back</span>
          </label>

          {enabled ? (
            <div className="flex flex-col gap-3 pl-5">
              <div className="flex flex-col gap-1.5">
                <span className="text-zinc-400">Remind me in</span>
                <div className="flex flex-wrap items-center gap-1.5">
                  {DUE_CHIPS.map((chip) => {
                    const selected = chipId === chip.id;
                    return (
                      <button
                        key={chip.id}
                        type="button"
                        disabled={disabled}
                        title={chip.description}
                        aria-pressed={selected}
                        onClick={() => setChipId(chip.id)}
                        className={`rounded-full px-2.5 py-1 font-medium transition ${
                          selected
                            ? "bg-zinc-800 text-zinc-100 ring-1 ring-inset ring-zinc-600"
                            : "text-zinc-400 hover:bg-zinc-900 hover:text-zinc-200"
                        }`}
                      >
                        {chip.label}
                      </button>
                    );
                  })}
                  <button
                    type="button"
                    disabled={disabled}
                    aria-pressed={isCustom}
                    onClick={() => setChipId("custom")}
                    className={`rounded-full px-2.5 py-1 font-medium transition ${
                      isCustom
                        ? "bg-zinc-800 text-zinc-100 ring-1 ring-inset ring-zinc-600"
                        : "text-zinc-400 hover:bg-zinc-900 hover:text-zinc-200"
                    }`}
                  >
                    Custom…
                  </button>
                </div>
                {isCustom ? (
                  <div className="flex flex-col gap-1">
                    <input
                      type="date"
                      value={customDate}
                      min={today}
                      disabled={disabled}
                      onChange={(e) => setCustomDate(e.target.value)}
                      className="w-40 rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1 text-zinc-100"
                    />
                    {dueError ? (
                      <span className="text-amber-300">{dueError}</span>
                    ) : null}
                  </div>
                ) : (
                  <span className="text-zinc-500">
                    Due {dueDate} (business days skip weekends)
                  </span>
                )}
              </div>

              <label className="flex flex-col gap-1">
                <span className="text-zinc-400">Reminder title</span>
                <input
                  type="text"
                  value={title}
                  disabled={disabled}
                  onChange={(e) => setTitle(e.target.value)}
                  className="rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1 text-zinc-100"
                />
              </label>

              <label className="flex flex-col gap-1">
                <span className="text-zinc-400">Note (optional)</span>
                <textarea
                  value={note}
                  disabled={disabled}
                  rows={2}
                  onChange={(e) => setNote(e.target.value)}
                  className="rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1 text-zinc-100"
                />
              </label>

              <label
                className="flex items-center gap-2 text-zinc-500"
                title="Coming soon — for now you'll draft the follow-up reply yourself when it's due."
              >
                <input
                  type="checkbox"
                  checked={false}
                  disabled
                  readOnly
                  className="h-3.5 w-3.5 accent-sky-500"
                />
                <span>
                  Also pre-draft a follow-up email when due
                  <span className="ml-1 rounded bg-zinc-800 px-1.5 py-0.5 text-[10px] text-zinc-400">
                    Coming soon
                  </span>
                </span>
              </label>

              <p className="text-zinc-500">
                We&rsquo;ll remind you to follow up if you don&rsquo;t hear back.
                We never send on your behalf.
              </p>
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
