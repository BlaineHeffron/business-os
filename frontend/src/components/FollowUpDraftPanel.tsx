import { useState } from "react";
import type { FollowUpDraftWithRevision } from "../types/generated/FollowUpDraftWithRevision";
import { api, errorMessage, isRevisionConflict, isUnauthorized } from "../lib/api";
import { StatusBadge } from "./ui";
import DraftFieldInput from "./DraftFieldInput";
import {
  useDraftPanel,
  useDraftEdit,
  DraftPanelShell,
  DraftEmptyCta,
  DraftStatusHeader,
  DraftActionFooter,
} from "./draft";

type TaskEdit = { title: string; due_date: string; context: string };

/** Detail panel under an accepted queue row for the follow_up_task kind:
 * produce the task draft, edit the AI-filled fields in place (source quotes
 * alongside), then approve (creates the local task immediately — no provider
 * involved) or reject. A dirty edit is saved as part of Approve. */
export default function FollowUpDraftPanel({
  itemId,
  onUnauthorized,
}: {
  itemId: string;
  onUnauthorized: () => void;
}) {
  const { drafts, loaded, active, producing, busy, notice, setNotice, produce, runAction, load } =
    useDraftPanel<FollowUpDraftWithRevision>({
      itemId,
      produceKind: "follow_up_task",
      onUnauthorized,
      fetchDrafts: (id) => api.followUpDrafts(id),
      produceDraft: (req) => api.produceFollowUpDraft(req),
      actionDraft: (draftId, req) => api.followUpDraftAction(draftId, req),
      produceTimeoutText:
        "The draft didn't finish after 3 minutes — drafting may have failed (check AI Usage). Try again.",
    });

  const [edit, setEdit] = useDraftEdit<FollowUpDraftWithRevision, TaskEdit>(
    active,
    (entry) => ({
      title: entry.draft.title,
      due_date: entry.draft.due_date ?? "",
      context: entry.draft.context,
    }),
  );
  const [savingEdits, setSavingEdits] = useState(false);

  const dirty =
    active != null &&
    edit != null &&
    (edit.title !== active.draft.title ||
      edit.due_date !== (active.draft.due_date ?? "") ||
      edit.context !== active.draft.context);
  const draftInvalid = edit == null || edit.title.trim() === "";

  const quoteFor = (field: string) =>
    active?.draft.provenance.find((p) => p.field === field)?.quote ?? "";

  const saveEdits = async (revision: number): Promise<number> => {
    const saved = await api.updateFollowUpDraft(active!.draft.draft_id, {
      title: edit!.title,
      due_date: edit!.due_date.trim() === "" ? null : edit!.due_date.trim(),
      context: edit!.context,
      expected_revision: revision,
      idempotency_key: crypto.randomUUID(),
      actor_id: null,
    });
    return saved.revision ?? revision + 1;
  };

  const saveDraft = async () => {
    if (!active || !dirty) return;
    setSavingEdits(true);
    setNotice(null);
    try {
      await saveEdits(active.revision);
      await load();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else if (isRevisionConflict(err)) {
        setNotice({ text: "Draft changed elsewhere — reloaded.", kind: "conflict" });
        await load();
      } else {
        setNotice({ text: `Save failed: ${errorMessage(err)}`, kind: "error" });
      }
    } finally {
      setSavingEdits(false);
    }
  };

  return (
    <DraftPanelShell loaded={loaded} notice={notice}>
      {!active ? (
        <DraftEmptyCta
          message="No task draft yet — draft one from this email with AI, then review before it lands on your task list."
          buttonLabel="Draft task"
          busyLabel="Drafting…"
          producing={producing}
          onProduce={() => void produce()}
          historyCount={drafts.length}
        />
      ) : (
        <div className="flex flex-col gap-2">
          <DraftStatusHeader
            status={active.draft.status}
            confidence={active.draft.confidence}
            model={active.draft.model}
          />

          {active.draft.status === "staged" && edit ? (
            <div className="flex max-w-xl flex-col gap-2">
              <DraftFieldInput
                label="Task"
                value={edit.title}
                onChange={(title) => setEdit({ ...edit, title })}
                quote={quoteFor("title")}
                maxLength={200}
                disabled={busy || savingEdits}
              />
              <DraftFieldInput
                label="Due"
                value={edit.due_date}
                onChange={(due_date) => setEdit({ ...edit, due_date })}
                quote={quoteFor("due_date")}
                hint="YYYY-MM-DD, blank = no deadline"
                placeholder="no deadline"
                maxLength={64}
                disabled={busy || savingEdits}
              />
              <DraftFieldInput
                label="Context"
                value={edit.context}
                onChange={(context) => setEdit({ ...edit, context })}
                quote={quoteFor("context")}
                multiline
                maxLength={1_000}
                disabled={busy || savingEdits}
              />
            </div>
          ) : (
            <div className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
              {(
                [
                  ["Task", active.draft.title, quoteFor("title")],
                  ["Due", active.draft.due_date ?? "no deadline", quoteFor("due_date")],
                  ["Context", active.draft.context || "—", quoteFor("context")],
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
                    ) : null}
                  </span>
                </div>
              ))}
            </div>
          )}

          <DraftActionFooter
            visible={active.draft.status === "staged"}
            busy={busy}
            saving={savingEdits}
            dirty={dirty}
            saveDisabled={draftInvalid}
            saveTitle={draftInvalid ? "Add a task title before saving." : undefined}
            approveDisabled={draftInvalid}
            approveLabel="Approve → task list"
            approveDirtyLabel="Save & approve → task list"
            approveTitle={
              draftInvalid
                ? "Add a task title before approval."
                : "Creates the task on your task list (local write, immediate)"
            }
            onSave={() => void saveDraft()}
            onApprove={() =>
              void runAction(
                active,
                "approve",
                dirty && edit ? saveEdits : undefined,
              )
            }
            onReject={() => void runAction(active, "reject")}
            onResetEdits={() =>
              setEdit({
                title: active.draft.title,
                due_date: active.draft.due_date ?? "",
                context: active.draft.context,
              })
            }
          />

          {active.draft.status === "approved" ? (
            <div className="flex items-center gap-2 text-xs">
              <StatusBadge tone="ok">approved</StatusBadge>
              <span className="text-xs text-zinc-400">
                Task created — see the Tasks tab
                {active.draft.task_id ? ` (${active.draft.task_id})` : ""}
              </span>
            </div>
          ) : null}
        </div>
      )}
    </DraftPanelShell>
  );
}
