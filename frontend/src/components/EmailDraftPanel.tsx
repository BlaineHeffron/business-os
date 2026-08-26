import { useRef, useState } from "react";
import type { EmailDraftWithRevision } from "../types/generated/EmailDraftWithRevision";
import {
  api,
  errorMessage,
  isRevisionConflict,
  isUnauthorized,
} from "../lib/api";
import {
  buildFollowUpRequestBody,
  followUpFooterLabels,
  type FollowUpDecision,
} from "../lib/followUp";
import DraftFieldInput from "./DraftFieldInput";
import {
  useDraftPanel,
  useDraftEdit,
  DraftPanelShell,
  DraftEmptyCta,
  DraftStatusHeader,
  DraftActionFooter,
  FollowUpSection,
  OutboxStateLine,
} from "./draft";
import { Button } from "./ui";

type EmailDraftEdit = {
  to_addr: string;
  cc_addrs: string;
  subject: string;
  body_text: string;
};

/** Detail panel under an accepted queue row for the email_draft_reply kind:
 * produce the reply draft, edit its typed fields in place, optionally rewrite
 * the body with AI, then approve —
 * which creates a Gmail DRAFT (never sends; dry-run while the gate is closed)
 * — or reject. A dirty edit is saved as part of Approve. */
export default function EmailDraftPanel({
  itemId,
  onUnauthorized,
}: {
  itemId: string;
  onUnauthorized: () => void;
}) {
  // Optional follow-up reminder attached at approval time (plan §5.1). Default
  // OFF; the FollowUpSection bubbles its resolved decision up here so we can
  // relabel the footer and ride the payload along on the single approve action.
  const [followUp, setFollowUp] = useState<FollowUpDecision>({
    enabled: false,
    valid: true,
    dueDate: "",
    title: "",
    note: "",
  });
  // Keep the live payload in a ref so the (stable) actionDraft closure below
  // reads the latest decision without re-subscribing the panel hook.
  const followUpPayload = buildFollowUpRequestBody(followUp);
  const followUpRef = useRef(followUpPayload);
  followUpRef.current = followUpPayload;

  const { drafts, loaded, active, producing, busy, notice, setNotice, produce, runAction, load } =
    useDraftPanel<EmailDraftWithRevision>({
      itemId,
      produceKind: "email_draft_reply",
      onUnauthorized,
      fetchDrafts: (id) => api.emailDrafts(id),
      produceDraft: (req) => api.produceEmailDraft(req),
      // The follow-up reminder rides along on approve only — one atomic action.
      actionDraft: (draftId, req) =>
        api.emailDraftAction(draftId, {
          ...req,
          ...(req.action === "approve" && followUpRef.current
            ? { follow_up: followUpRef.current }
            : {}),
        }),
      produceTimeoutText:
        "The draft didn't finish after 3 minutes — drafting may have failed (check AI Usage). Try again.",
    });

  const [edit, setEdit] = useDraftEdit<EmailDraftWithRevision, EmailDraftEdit>(
    active,
    (entry) => ({
      to_addr: entry.draft.to_addr,
      cc_addrs: (entry.draft.cc_addrs ?? []).join(", "),
      subject: entry.draft.subject,
      body_text: entry.draft.body_text,
    }),
  );
  const [rewriteInstructions, setRewriteInstructions] = useState("");
  const [rewriting, setRewriting] = useState(false);
  const [savingEdits, setSavingEdits] = useState(false);

  const dirty =
    active != null &&
    edit != null &&
    (edit.to_addr !== active.draft.to_addr ||
      edit.cc_addrs !== (active.draft.cc_addrs ?? []).join(", ") ||
      edit.subject !== active.draft.subject ||
      edit.body_text !== active.draft.body_text);
  const draftInvalid =
    edit == null ||
    edit.to_addr.trim() === "" ||
    edit.subject.trim() === "" ||
    edit.body_text.trim() === "";

  const splitRecipients = (raw: string) =>
    raw
      .split(",")
      .map((entry) => entry.trim())
      .filter(Boolean);

  const saveEdits = async (revision: number): Promise<number> => {
    const saved = await api.updateEmailDraft(active!.draft.draft_id, {
      to_addr: edit!.to_addr,
      cc_addrs: splitRecipients(edit!.cc_addrs),
      subject: edit!.subject,
      body_text: edit!.body_text,
      expected_revision: revision,
      idempotency_key: crypto.randomUUID(),
      actor_id: null,
    });
    return saved.revision ?? revision + 1;
  };

  const rewrite = async () => {
    if (!active || !edit || rewriteInstructions.trim() === "") return;
    setRewriting(true);
    setNotice(null);
    try {
      const revision = dirty ? await saveEdits(active.revision) : active.revision;
      await api.rewriteEmailDraft(active.draft.draft_id, {
        instructions: rewriteInstructions.trim(),
        expected_revision: revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setRewriteInstructions("");
      await load();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else if (isRevisionConflict(err)) {
        setNotice({ text: "Draft changed elsewhere — reloaded.", kind: "conflict" });
        await load();
      } else {
        setNotice({ text: `AI rewrite failed: ${errorMessage(err)}`, kind: "error" });
      }
    } finally {
      setRewriting(false);
    }
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

  const quoteFor = (field: string) =>
    active?.draft.provenance.find((p) => p.field === field)?.quote ?? "";

  return (
    <DraftPanelShell loaded={loaded} notice={notice}>
      {!active ? (
        <DraftEmptyCta
          message="No reply draft yet — draft one with AI, then review before a draft is created in Gmail (sending always stays with you)."
          buttonLabel="Draft reply"
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

          {active.draft.status === "staged" && edit != null ? (
            <div className="grid gap-3">
              <label className="flex flex-col gap-0.5">
                <span className="text-xs font-medium text-zinc-400">To</span>
                <input
                  value={edit.to_addr}
                  onChange={(event) =>
                    setEdit((current) =>
                      current ? { ...current, to_addr: event.target.value } : current,
                    )
                  }
                  disabled={busy || rewriting || savingEdits}
                  className="w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-sm text-zinc-200 placeholder:text-zinc-500 focus:border-sky-600 focus:outline-none disabled:opacity-40"
                  placeholder="recipient@example.com"
                  maxLength={2_000}
                />
              </label>
              <label className="flex flex-col gap-0.5">
                <span className="text-xs font-medium text-zinc-400">Cc</span>
                <input
                  value={edit.cc_addrs}
                  onChange={(event) =>
                    setEdit((current) =>
                      current ? { ...current, cc_addrs: event.target.value } : current,
                    )
                  }
                  disabled={busy || rewriting || savingEdits}
                  className="w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-sm text-zinc-200 placeholder:text-zinc-500 focus:border-sky-600 focus:outline-none disabled:opacity-40"
                  placeholder="optional@example.com"
                  maxLength={10_000}
                />
              </label>
              <label className="flex flex-col gap-0.5">
                <span className="text-xs font-medium text-zinc-400">Subject</span>
                <input
                  value={edit.subject}
                  onChange={(event) =>
                    setEdit((current) =>
                      current ? { ...current, subject: event.target.value } : current,
                    )
                  }
                  disabled={busy || rewriting || savingEdits}
                  className="w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-sm text-zinc-200 placeholder:text-zinc-500 focus:border-sky-600 focus:outline-none disabled:opacity-40"
                  placeholder="Email subject"
                  maxLength={500}
                />
              </label>
              <DraftFieldInput
                label="Reply"
                value={edit.body_text}
                onChange={(body_text) =>
                  setEdit((current) =>
                    current ? { ...current, body_text } : current,
                  )
                }
                quote={quoteFor("body_text")}
                multiline
                rows={14}
                maxLength={10_000}
                disabled={busy || rewriting || savingEdits}
              />
              <div className="rounded-md border border-violet-500/20 bg-violet-500/5 p-3">
                <label className="flex flex-col gap-1">
                  <span className="text-xs font-medium text-violet-200">
                    Optional AI rewrite
                  </span>
                  <textarea
                    value={rewriteInstructions}
                    onChange={(event) => setRewriteInstructions(event.target.value)}
                    rows={2}
                    maxLength={4_000}
                    disabled={busy || rewriting || savingEdits}
                    placeholder="Make this warmer, shorten it, or draft from the governed source context…"
                    className="w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-200 placeholder:text-zinc-500 focus:border-violet-500 focus:outline-none disabled:opacity-40"
                  />
                </label>
                <div className="mt-2 flex items-center justify-between gap-3">
                  <span className="text-xs text-zinc-400">
                    Saves your current fields first. AI changes the body only; approval stays with you.
                  </span>
                  <Button
                    variant="secondary"
                    size="sm"
                    busy={rewriting}
                    disabled={busy || rewriting || savingEdits || rewriteInstructions.trim() === ""}
                    onClick={() => void rewrite()}
                  >
                    {rewriting ? "Rewriting…" : "Rewrite with AI"}
                  </Button>
                </div>
              </div>
            </div>
          ) : (
            <div className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
              <span className="text-zinc-400">To</span>
              <span className="break-words text-zinc-200">
                {active.draft.to_addr}
              </span>
              {active.draft.cc_addrs && active.draft.cc_addrs.length > 0 ? (
                <>
                  <span className="text-zinc-400">Cc</span>
                  <span className="break-words text-zinc-200">
                    {active.draft.cc_addrs.join(", ")}
                  </span>
                </>
              ) : null}
              <span className="text-zinc-400">Subject</span>
              <span className="break-words text-zinc-200">
                {active.draft.subject}
              </span>
              <span className="text-zinc-400">Reply</span>
              <span className="whitespace-pre-wrap text-zinc-200">
                {active.draft.body_text}
              </span>
            </div>
          )}

          {active.draft.status === "staged" ? (
            <FollowUpSection
              key={active.draft.draft_id}
              subject={active.draft.subject}
              disabled={busy || rewriting || savingEdits}
              onChange={setFollowUp}
            />
          ) : null}

          <DraftActionFooter
            visible={active.draft.status === "staged"}
            busy={busy || rewriting}
            saving={savingEdits}
            dirty={dirty}
            saveDisabled={draftInvalid}
            saveTitle={draftInvalid ? "Complete To, Subject, and Body before saving." : undefined}
            approveDisabled={(followUp.enabled && !followUp.valid) || draftInvalid}
            approveLabel={followUpFooterLabels(followUp.enabled).approve}
            approveDirtyLabel={followUpFooterLabels(followUp.enabled).approveDirty}
            approveTitle={
              draftInvalid
                ? "Complete To, Subject, and Body before approval."
                : "Creates the Gmail draft when you approve — sending stays with you."
            }
            onSave={() => void saveDraft()}
            onApprove={() =>
              void runAction(
                active,
                "approve",
                dirty ? saveEdits : undefined,
              )
            }
            onReject={() => void runAction(active, "reject")}
            onResetEdits={() =>
              setEdit({
                to_addr: active.draft.to_addr,
                cc_addrs: (active.draft.cc_addrs ?? []).join(", "),
                subject: active.draft.subject,
                body_text: active.draft.body_text,
              })
            }
          />

          <OutboxStateLine
            job={active.outbox_job}
            show={active.draft.status === "approved"}
            dryRunText="Tested successfully, but live Gmail drafts are turned off — ask your administrator to enable them."
            deliveredText={(job) =>
              `Gmail draft created${job.provider_object_id ? ` (${job.provider_object_id})` : ""} — open Gmail to review and send`
            }
            onUnauthorized={onUnauthorized}
            onRetried={load}
          />
        </div>
      )}
    </DraftPanelShell>
  );
}
