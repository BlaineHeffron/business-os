import type { CrmDraftWithRevision } from "../types/generated/CrmDraftWithRevision";
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

type NoteEdit = { note_body: string; contact_email: string };

/** Detail panel under an accepted queue row for the crm_activity kind:
 * produce the CRM note draft, edit the AI-filled fields in place (source
 * quotes alongside; logged-at stays grounded from the email date), then
 * approve (stages the HubSpot write — dry-run while the gate is closed) or
 * reject. A dirty edit is saved as part of Approve. */
export default function CrmDraftPanel({
  itemId,
  onUnauthorized,
}: {
  itemId: string;
  onUnauthorized: () => void;
}) {
  const { drafts, loaded, active, producing, busy, notice, produce, runAction, load } =
    useDraftPanel<CrmDraftWithRevision>({
      itemId,
      produceKind: "crm_activity",
      onUnauthorized,
      fetchDrafts: (id) => api.crmDrafts(id),
      produceDraft: (req) => api.produceCrmDraft(req),
      actionDraft: (draftId, req) => api.crmDraftAction(draftId, req),
      produceTimeoutText:
        "The draft didn't finish after 3 minutes — drafting may have failed (check AI Usage). Try again.",
    });

  const [edit, setEdit] = useDraftEdit<CrmDraftWithRevision, NoteEdit>(
    active,
    (entry) => ({
      note_body: entry.draft.note_body,
      contact_email: entry.draft.contact_email ?? "",
    }),
  );

  const dirty =
    active != null &&
    edit != null &&
    (edit.note_body !== active.draft.note_body ||
      edit.contact_email !== (active.draft.contact_email ?? ""));

  const quoteFor = (field: string) =>
    active?.draft.provenance.find((p) => p.field === field)?.quote ?? "";

  return (
    <DraftPanelShell loaded={loaded} notice={notice}>
      {!active ? (
        <DraftEmptyCta
          message="No CRM note draft yet — draft one from this message with AI, then review before anything is written to the CRM."
          buttonLabel="Draft CRM note"
          busyLabel="Drafting…"
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
                label="Note"
                value={edit.note_body}
                onChange={(note_body) => setEdit({ ...edit, note_body })}
                quote={quoteFor("note_body")}
                multiline
                disabled={busy}
              />
              <DraftFieldInput
                label="Contact"
                value={edit.contact_email}
                onChange={(contact_email) =>
                  setEdit({ ...edit, contact_email })
                }
                quote={quoteFor("contact_email")}
                placeholder="no contact email"
                disabled={busy}
              />
              <div className="text-xs text-zinc-400">
                Logged at{" "}
                <span className="text-zinc-200">
                  {new Date(active.draft.occurred_at).toLocaleString()}
                </span>{" "}
                <span className="text-zinc-500">
                  (taken from the message date — not editable)
                </span>
              </div>
            </div>
          ) : (
            <div className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
              {(
                [
                  ["Note", active.draft.note_body, quoteFor("note_body")],
                  [
                    "Contact",
                    active.draft.contact_email ?? "—",
                    quoteFor("contact_email"),
                  ],
                  [
                    "Logged at",
                    new Date(active.draft.occurred_at).toLocaleString(),
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
                        title="Source quote from the message"
                      >
                        "{quote}"
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
            approveLabel="Approve → CRM"
            approveDirtyLabel="Save & approve → CRM"
            approveTitle="Adds this note to your CRM when you approve."
            onApprove={() =>
              void runAction(
                active,
                "approve",
                dirty && edit
                  ? async (revision) => {
                      const saved = await api.updateCrmDraft(
                        active.draft.draft_id,
                        {
                          note_body: edit.note_body,
                          contact_email:
                            edit.contact_email.trim() === ""
                              ? null
                              : edit.contact_email.trim(),
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
            onResetEdits={() =>
              setEdit({
                note_body: active.draft.note_body,
                contact_email: active.draft.contact_email ?? "",
              })
            }
          />

          <OutboxStateLine
            job={active.outbox_job}
            show={active.draft.status === "approved"}
            dryRunText="Tested successfully, but live CRM writes are turned off — ask your administrator to enable them."
            deliveredText={(job) =>
              `Note created in the CRM${job.provider_object_id ? ` (${job.provider_object_id})` : ""}`
            }
            onUnauthorized={onUnauthorized}
            onRetried={load}
          />
        </div>
      )}
    </DraftPanelShell>
  );
}
