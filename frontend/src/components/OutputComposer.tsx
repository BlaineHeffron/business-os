import { useMemo, useRef, useState } from "react";
import { api, errorMessage, isUnauthorized } from "../lib/api";
import OutputComposerShell from "./output/OutputComposerShell";
import DraftFieldInput from "./DraftFieldInput";
import { Button, StatusBadge } from "./ui";

export type OutputKind = "email_draft_reply" | "follow_up_task";
type ComposerMode = "manual" | "ai";

const KIND_TABS = [
  { id: "email_draft_reply", label: "Email" },
  { id: "follow_up_task", label: "Follow-up task" },
];

export function canCloseComposer(
  values: readonly string[],
  workItemCreated: boolean,
  confirmDiscard: () => boolean,
): boolean {
  const hasUnsavedInput = values.some((value) => value.trim().length > 0);
  return workItemCreated || !hasUnsavedInput || confirmDiscard();
}

function splitRecipients(raw: string): string[] {
  return raw
    .split(",")
    .map((value) => value.trim())
    .filter(Boolean);
}

/** Blank/manual mode for the same typed draft owners used by Queue. It first
 * creates the governed operator-note source + accepted item, then stages in
 * the selected owning slice. AI is opt-in and never approves or delivers. */
export default function OutputComposer({
  onClose,
  onCreated,
  onUnauthorized,
  availableKinds,
}: {
  onClose: () => void;
  onCreated: (itemId: string) => void;
  onUnauthorized: () => void;
  availableKinds: OutputKind[];
}) {
  const [kind, setKind] = useState<OutputKind>(availableKinds[0] ?? "email_draft_reply");
  const [mode, setMode] = useState<ComposerMode>("manual");
  const [context, setContext] = useState("");
  const [instructions, setInstructions] = useState("");
  const [toAddr, setToAddr] = useState("");
  const [ccAddrs, setCcAddrs] = useState("");
  const [subject, setSubject] = useState("");
  const [emailBody, setEmailBody] = useState("");
  const [taskTitle, setTaskTitle] = useState("");
  const [taskDueDate, setTaskDueDate] = useState("");
  const [taskContext, setTaskContext] = useState("");
  const [saving, setSaving] = useState(false);
  const [started, setStarted] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const rootKey = useRef(crypto.randomUUID());
  const workItemCreated = useRef(false);
  const createdItemId = useRef<string | null>(null);
  const editingLocked = saving;
  const requestClose = () => {
    if (createdItemId.current) {
      onCreated(createdItemId.current);
      return;
    }
    if (
      !canCloseComposer(
        [
          context,
          instructions,
          toAddr,
          ccAddrs,
          subject,
          emailBody,
          taskTitle,
          taskDueDate,
          taskContext,
        ],
        workItemCreated.current,
        () => window.confirm("Discard this output and its unsaved content?"),
      )
    ) {
      return;
    }
    onClose();
  };

  const validation = useMemo(() => {
    if (kind === "email_draft_reply") {
      if (!toAddr.trim()) return "Add at least one recipient.";
      if (!subject.trim()) return "Add a subject.";
      if (mode === "manual" && !emailBody.trim()) return "Write the email body.";
      if (mode === "ai" && !instructions.trim()) return "Tell AI what to draft or rewrite.";
      return null;
    }
    if (mode === "manual" && !taskTitle.trim()) return "Add a task title.";
    if (mode === "ai" && !instructions.trim()) return "Tell AI what task to prepare.";
    return null;
  }, [emailBody, instructions, kind, mode, subject, taskTitle, toAddr]);

  const sourceBody = () => {
    const blocks = [
      kind === "email_draft_reply"
        ? [
            "Create a new email output.",
            `To: ${toAddr.trim()}`,
            ccAddrs.trim() ? `Cc: ${ccAddrs.trim()}` : "",
            `Subject: ${subject.trim()}`,
            emailBody.trim() ? `Current body:\n${emailBody.trim()}` : "",
          ]
            .filter(Boolean)
            .join("\n")
        : [
            "Create a follow-up task.",
            taskTitle.trim() ? `Task: ${taskTitle.trim()}` : "",
            taskDueDate.trim() ? `Due: ${taskDueDate.trim()}` : "",
            taskContext.trim() ? `Task context:\n${taskContext.trim()}` : "",
          ]
            .filter(Boolean)
            .join("\n"),
      context.trim() ? `Governed operator context:\n${context.trim()}` : "",
      instructions.trim() ? `Operator AI instruction:\n${instructions.trim()}` : "",
    ].filter(Boolean);
    return blocks.join("\n\n");
  };

  const create = async () => {
    if (validation) return;
    setStarted(true);
    setSaving(true);
    setError(null);
    try {
      const idempotencyRoot = rootKey.current;
      const shouldAutoProduce = mode === "ai" && kind === "follow_up_task";
      const note = await api.createOperatorNote({
        body: sourceBody(),
        idempotency_key: idempotencyRoot,
        actor_id: null,
        actions: [kind],
        auto_produce: shouldAutoProduce,
      });
      workItemCreated.current = true;
      createdItemId.current = note.work_item_id;

      if (kind === "email_draft_reply") {
        const staged = await api.stageManualEmailDraft({
          item_id: note.work_item_id,
          to_addr: toAddr.trim(),
          cc_addrs: splitRecipients(ccAddrs),
          subject: subject.trim(),
          body_text: emailBody.trim(),
          idempotency_key: `${idempotencyRoot}:stage:email`,
          actor_id: null,
        });
        if (mode === "ai") {
          await api.rewriteEmailDraft(staged.draft.draft.draft_id, {
            instructions: instructions.trim(),
            expected_revision: staged.draft.revision,
            idempotency_key: `${idempotencyRoot}:rewrite:email`,
            actor_id: null,
          });
        }
      } else if (mode === "manual") {
        await api.stageManualFollowUpDraft({
          item_id: note.work_item_id,
          title: taskTitle.trim(),
          due_date: taskDueDate.trim() || null,
          context: taskContext.trim(),
          idempotency_key: `${idempotencyRoot}:stage:follow-up`,
          actor_id: null,
        });
      }

      onCreated(note.work_item_id);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    } finally {
      setSaving(false);
    }
  };

  const contextPanel = (
    <div className="space-y-3 p-4">
      <div>
        <StatusBadge tone="info">operator supplied</StatusBadge>
        <p className="mt-2 text-xs text-zinc-400">
          No inbound artifact required. This bounded context is saved as the
          governed source behind the Queue item. Credentials never enter it.
        </p>
      </div>
      <label className="flex flex-col gap-1">
        <span className="text-xs font-medium text-zinc-400">Optional context</span>
        <textarea
          value={context}
          onChange={(event) => setContext(event.target.value)}
          rows={10}
          maxLength={12_000}
          disabled={editingLocked}
          placeholder="Customer, project, facts, constraints, or pasted source text…"
          className="w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-200 placeholder:text-zinc-500 focus:border-sky-600 focus:outline-none disabled:opacity-40"
        />
      </label>
    </div>
  );

  const footer = (
    <div className="flex flex-wrap items-center justify-between gap-3">
      <div className="text-xs text-zinc-400">
        {validation ??
          (mode === "ai"
            ? "AI stages a proposal only. Review and exact-revision approval remain separate."
            : "Stages a typed draft without spending an AI call.")}
      </div>
      <div className="flex items-center gap-2">
        <Button variant="ghost" size="sm" disabled={saving} onClick={requestClose}>
          Cancel
        </Button>
        <Button
          variant="primary"
          size="sm"
          busy={saving}
          disabled={saving || validation != null}
          onClick={() => void create()}
        >
          {saving
            ? mode === "ai"
              ? "Drafting…"
              : "Creating…"
            : mode === "ai"
              ? "Create & draft with AI"
              : "Create output"}
        </Button>
      </div>
    </div>
  );

  return (
    <OutputComposerShell
      title="Create output"
      mode="blank"
      tabs={KIND_TABS.filter((tab) => availableKinds.includes(tab.id as OutputKind))}
      activeTab={kind}
      onSelectTab={(id) => {
        if (!started) setKind(id as OutputKind);
      }}
      tabsDisabled={started}
      contextTitle="Governed context"
      context={contextPanel}
      footer={footer}
      onClose={requestClose}
    >
      <div className="mx-auto flex max-w-4xl flex-col gap-5 p-4 sm:p-6">
        <div className="flex items-center gap-1 rounded-md border border-zinc-800 bg-zinc-900/40 p-1 self-start">
          {(["manual", "ai"] as const).map((candidate) => (
            <button
              key={candidate}
              onClick={() => setMode(candidate)}
              disabled={saving || started}
              className={`rounded px-3 py-1.5 text-xs font-medium transition focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70 ${
                mode === candidate
                  ? candidate === "ai"
                    ? "bg-violet-500/15 text-violet-200"
                    : "bg-sky-500/15 text-sky-200"
                  : "text-zinc-400 hover:bg-zinc-800 hover:text-zinc-200"
              }`}
            >
              {candidate === "manual" ? "Write manually" : "Draft with AI"}
            </button>
          ))}
        </div>
        <div className="flex flex-wrap items-center gap-2 text-xs text-zinc-400">
          <StatusBadge tone="neutral">
            {kind === "email_draft_reply" ? "Gmail draft" : "BusinessOS Tasks"}
          </StatusBadge>
          <span>
            {kind === "email_draft_reply"
              ? "Approval creates a Gmail draft; BusinessOS never sends it."
              : "Approval creates the local task immediately."}
          </span>
        </div>

        {error ? (
          <div role="alert" className="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300">
            Output creation failed: {error}. Retry resumes this same safe request;
            cancel to start over with different fields.
          </div>
        ) : null}

        {kind === "email_draft_reply" ? (
          <div className="grid gap-3">
            <DraftFieldInput
              label="To"
              value={toAddr}
              onChange={setToAddr}
              quote=""
              showProvenance={false}
              maxLength={2_000}
              disabled={editingLocked}
            />
            <DraftFieldInput
              label="Cc"
              value={ccAddrs}
              onChange={setCcAddrs}
              quote=""
              hint="Optional, comma-separated"
              showProvenance={false}
              maxLength={10_000}
              disabled={editingLocked}
            />
            <DraftFieldInput
              label="Subject"
              value={subject}
              onChange={setSubject}
              quote=""
              showProvenance={false}
              maxLength={500}
              disabled={editingLocked}
            />
            <DraftFieldInput
              label={mode === "ai" ? "Starting draft (optional)" : "Body"}
              value={emailBody}
              onChange={setEmailBody}
              quote=""
              multiline
              rows={14}
              showProvenance={false}
              maxLength={10_000}
              disabled={editingLocked}
            />
          </div>
        ) : (
          <div className="grid gap-3">
            <DraftFieldInput
              label={mode === "ai" ? "Starting task title (optional)" : "Task"}
              value={taskTitle}
              onChange={setTaskTitle}
              quote=""
              showProvenance={false}
              maxLength={200}
              disabled={editingLocked}
            />
            <DraftFieldInput
              label="Due"
              value={taskDueDate}
              onChange={setTaskDueDate}
              quote=""
              hint="YYYY-MM-DD, blank = no deadline"
              showProvenance={false}
              maxLength={64}
              disabled={editingLocked}
            />
            <DraftFieldInput
              label="Task context"
              value={taskContext}
              onChange={setTaskContext}
              quote=""
              multiline
              rows={8}
              showProvenance={false}
              maxLength={1_000}
              disabled={editingLocked}
            />
          </div>
        )}

        {mode === "ai" ? (
          <DraftFieldInput
            label="AI instructions"
            value={instructions}
            onChange={setInstructions}
            quote=""
            multiline
            rows={4}
            hint="Bounded typed generation. The model cannot approve or reach providers."
            showProvenance={false}
            maxLength={4_000}
            disabled={editingLocked}
          />
        ) : null}
      </div>
    </OutputComposerShell>
  );
}
