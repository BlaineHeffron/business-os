import { useEffect, useState } from "react";
import type { CalendarDraftWithRevision } from "../types/generated/CalendarDraftWithRevision";
import type { CalendarListResponse } from "../types/generated/CalendarListResponse";
import { api } from "../lib/api";
import DraftFieldInput from "./DraftFieldInput";
import {
  useDraftPanel,
  useDraftEdit,
  DraftPanelShell,
  DraftEmptyCta,
  DraftStatusHeader,
  DraftActionFooter,
  OutboxStateLine,
} from "./draft";

type EventEdit = {
  title: string;
  start_at: string;
  end_at: string;
  location: string;
  description: string;
  /** "" = the server default calendar. */
  calendar_id: string;
  attendees: string;
  send_invitations: boolean;
};

/** Linear-style detail panel under an accepted queue row: produce the event
 * draft, edit the AI-extracted fields in place (source quotes alongside),
 * then approve or reject. A dirty edit is saved as part of Approve. Approval
 * stages the provider write; the delivery state line shows whether it ran
 * for real or dry-run (write gate closed). */
export default function CalendarDraftPanel({
  itemId,
  onUnauthorized,
}: {
  itemId: string;
  onUnauthorized: () => void;
}) {
  const { drafts, loaded, active, producing, busy, notice, produce, runAction, load } =
    useDraftPanel<CalendarDraftWithRevision>({
      itemId,
      produceKind: "calendar_event_draft",
      onUnauthorized,
      fetchDrafts: (id) => api.calendarDrafts(id),
      produceDraft: (req) => api.produceCalendarDraft(req),
      actionDraft: (draftId, req) => api.calendarDraftAction(draftId, req),
      produceTimeoutText:
        "The draft didn't finish after 3 minutes — drafting may have failed (check AI Usage). Try again.",
    });

  const fromDraft = (entry: CalendarDraftWithRevision): EventEdit => ({
    title: entry.draft.title,
    start_at: entry.draft.start_at,
    end_at: entry.draft.end_at,
    location: entry.draft.location ?? "",
    description: entry.draft.description ?? "",
    calendar_id: entry.draft.calendar_id ?? "",
    attendees: entry.draft.attendees.join("\n"),
    send_invitations: entry.draft.send_invitations,
  });

  const [edit, setEdit] = useDraftEdit<CalendarDraftWithRevision, EventEdit>(
    active,
    fromDraft,
  );

  const [calendarChoices, setCalendarChoices] =
    useState<CalendarListResponse | null>(null);

  const dirty =
    active != null &&
    edit != null &&
    JSON.stringify(edit) !== JSON.stringify(fromDraft(active));

  // Load the calendar picker options once a staged draft is open. Failure
  // degrades to the default-only option — approving still works.
  const staged = active?.draft.status === "staged";
  useEffect(() => {
    if (!staged || calendarChoices !== null) return;
    let alive = true;
    api
      .calendarOptions()
      .then((res) => {
        if (alive) setCalendarChoices(res);
      })
      .catch(() => {
        // Quiet: picker shows only the default option.
      });
    return () => {
      alive = false;
    };
  }, [staged, calendarChoices]);

  const quoteFor = (field: string) =>
    active?.draft.provenance.find((p) => p.field === field)?.quote ?? "";

  const fmtWhen = (iso: string) => {
    const date = new Date(iso);
    return isNaN(date.getTime()) ? iso : date.toLocaleString();
  };

  return (
    <DraftPanelShell loaded={loaded} notice={notice}>
      {!active ? (
        <DraftEmptyCta
          message="No event draft yet — draft one from this email with AI, then review before anything is written."
          buttonLabel="Draft event"
          busyLabel="Extracting…"
          producing={producing}
          onProduce={() => void produce()}
          historyCount={drafts.length}
        />
      ) : (
        <div className="flex flex-col gap-2">
          <DraftStatusHeader
            status={active.draft.status}
            dryRun={active.outbox_job?.dry_run}
            confidence={active.draft.confidence}
            model={active.draft.model}
          />

          {active.draft.status === "staged" && edit ? (
            <div className="flex max-w-xl flex-col gap-2">
              <DraftFieldInput
                label="Title"
                value={edit.title}
                onChange={(title) => setEdit({ ...edit, title })}
                quote={quoteFor("title")}
                disabled={busy}
              />
              <DraftFieldInput
                label={`Starts (${fmtWhen(edit.start_at)})`}
                value={edit.start_at}
                onChange={(start_at) => setEdit({ ...edit, start_at })}
                quote={quoteFor("start_at")}
                hint="Date and time with timezone, e.g. 2026-06-12T16:00:00-04:00"
                disabled={busy}
              />
              <DraftFieldInput
                label={`Ends (${fmtWhen(edit.end_at)})`}
                value={edit.end_at}
                onChange={(end_at) => setEdit({ ...edit, end_at })}
                quote={quoteFor("end_at")}
                hint="Date and time with timezone"
                disabled={busy}
              />
              <DraftFieldInput
                label="Location"
                value={edit.location}
                onChange={(location) => setEdit({ ...edit, location })}
                quote={quoteFor("location")}
                placeholder="none"
                disabled={busy}
              />
              <DraftFieldInput
                label="Notes"
                value={edit.description}
                onChange={(description) => setEdit({ ...edit, description })}
                quote={quoteFor("description")}
                multiline
                placeholder="none"
                disabled={busy}
              />
              <div className="flex flex-col gap-0.5">
                <span className="text-xs font-medium text-zinc-400">
                  Calendar
                </span>
                <select
                  value={edit.calendar_id}
                  onChange={(e) =>
                    setEdit({ ...edit, calendar_id: e.target.value })
                  }
                  disabled={busy}
                  className="w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs text-zinc-200 focus:border-sky-600 focus:outline-none disabled:opacity-40"
                >
                  <option value="">
                    Default
                    {calendarChoices
                      ? ` (${calendarChoices.default_calendar_id})`
                      : ""}
                  </option>
                  {(calendarChoices?.calendars ?? []).map((cal) => (
                    <option key={cal.id} value={cal.id}>
                      {cal.summary || cal.id}
                      {cal.primary ? " — primary" : ""}
                    </option>
                  ))}
                </select>
                <span className="text-xs text-zinc-500">
                  Where the event is created on approval
                  {calendarChoices === null
                    ? " — calendar list unavailable, the default applies"
                    : ""}
                </span>
              </div>
              <div className="flex flex-col gap-0.5">
                <label
                  htmlFor={`calendar-attendees-${active.draft.draft_id}`}
                  className="text-xs font-medium text-zinc-400"
                >
                  Attendees
                </label>
                <textarea
                  id={`calendar-attendees-${active.draft.draft_id}`}
                  value={edit.attendees}
                  onChange={(event) => {
                    const attendees = event.target.value;
                    setEdit({
                      ...edit,
                      attendees,
                      send_invitations:
                        attendees.trim() === "" ? false : edit.send_invitations,
                    });
                  }}
                  disabled={busy}
                  rows={Math.max(2, Math.min(6, edit.attendees.split("\n").length))}
                  placeholder="One email address per line"
                  className="w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs text-zinc-200 focus:border-sky-600 focus:outline-none disabled:opacity-40"
                />
                <span className="text-xs text-zinc-500">
                  One address per line, up to 25. Remove a line to remove that attendee.
                </span>
              </div>
              <label className="flex items-start gap-2 rounded-md border border-zinc-800 bg-zinc-950/60 p-2">
                <input
                  type="checkbox"
                  checked={edit.send_invitations}
                  onChange={(event) =>
                    setEdit({ ...edit, send_invitations: event.target.checked })
                  }
                  disabled={busy || edit.attendees.trim() === ""}
                  className="mt-0.5"
                />
                <span className="text-xs text-zinc-300">
                  <span className="font-medium">Send calendar invitations</span>
                  <span className="mt-0.5 block text-zinc-500">
                    When live calendar writes are enabled, approval asks Google Calendar
                    to email every attendee listed above.
                  </span>
                </span>
              </label>
            </div>
          ) : (
            <div className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
              {(
                [
                  ["Title", active.draft.title, quoteFor("title")],
                  ["Starts", fmtWhen(active.draft.start_at), quoteFor("start_at")],
                  ["Ends", fmtWhen(active.draft.end_at), quoteFor("end_at")],
                  ["Location", active.draft.location ?? "—", quoteFor("location")],
                  [
                    "Notes",
                    active.draft.description ?? "—",
                    quoteFor("description"),
                  ],
                  [
                    "Calendar",
                    active.draft.calendar_id ?? "default",
                    "",
                  ],
                  [
                    "Attendees",
                    active.draft.attendees.length > 0
                      ? active.draft.attendees.join(", ")
                      : "—",
                    "",
                  ],
                  [
                    "Invitations",
                    active.draft.send_invitations ? "Send through Google Calendar" : "Do not send",
                    "",
                  ],
                ] as const
              ).map(([label, value, quote]) => (
                <div key={label} className="contents">
                  <span className="text-zinc-400">{label}</span>
                  <span className="text-zinc-200">
                    {value}
                    {quote ? (
                      <span
                        className="ml-2 text-xs italic text-zinc-500"
                        title="Source quote from the email"
                      >
                        "{quote}"
                      </span>
                    ) : !["Location", "Notes", "Calendar", "Attendees", "Invitations"].includes(label) ? (
                      <span className="ml-2 text-xs text-amber-400/80">
                        (inferred)
                      </span>
                    ) : null}
                  </span>
                </div>
              ))}
            </div>
          )}

          <DraftActionFooter
            visible={active.draft.status === "staged"}
            busy={busy}
            dirty={dirty}
            approveLabel="Approve → calendar"
            approveDirtyLabel="Save & approve → calendar"
            approveTitle={
              edit?.send_invitations
                ? "Creates the event and, when live writes are enabled, asks Google Calendar to email every attendee."
                : "Creates the event on your calendar when you approve."
            }
            onApprove={() =>
              void runAction(
                active,
                "approve",
                dirty
                  ? async (revision) => {
                      const saved = await api.updateCalendarDraft(
                        active.draft.draft_id,
                        {
                          title: edit!.title,
                          start_at: edit!.start_at,
                          end_at: edit!.end_at,
                          timezone: active.draft.timezone ?? null,
                          location: edit!.location.trim() === "" ? null : edit!.location,
                          description: edit!.description.trim() === "" ? null : edit!.description,
                          calendar_id: edit!.calendar_id.trim() === "" ? null : edit!.calendar_id,
                          attendees: edit!.attendees
                            .split("\n")
                            .map((address) => address.trim())
                            .filter((address) => address !== ""),
                          send_invitations: edit!.send_invitations,
                          expected_revision: revision,
                          idempotency_key: crypto.randomUUID(),
                          actor_id: null,
                        },
                      );
                      return saved.revision ?? revision + 1;
                    }
                  : undefined,
              )
            }
            onReject={() => void runAction(active, "reject")}
            onResetEdits={() => setEdit(fromDraft(active))}
          />

          <OutboxStateLine
            job={active.outbox_job}
            show={active.draft.status === "approved"}
            dryRunText="Tested successfully, but live calendar writes are turned off — ask your administrator to enable them."
            deliveredText={(job) =>
              `Event created on Google Calendar${job.provider_object_id ? ` (${job.provider_object_id})` : ""}`
            }
            onUnauthorized={onUnauthorized}
            onRetried={load}
          />
        </div>
      )}
    </DraftPanelShell>
  );
}
