import type { CrmSalesIntentDraftWithRevision } from "../types/generated/CrmSalesIntentDraftWithRevision";
import type { CrmSalesIntentProviderTarget } from "../types/generated/CrmSalesIntentProviderTarget";
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

type SalesIntentEdit = {
  company_name: string;
  contact_name: string;
  contact_email: string;
  lead_title: string;
  intent_summary: string;
  rationale: string;
  qualification_status: string;
  next_step_text: string;
  follow_up_due_date: string;
  provider_target: CrmSalesIntentProviderTarget;
  create_businessos_task: boolean;
};

export default function CrmSalesIntentPanel({
  itemId,
  onUnauthorized,
}: {
  itemId: string;
  onUnauthorized: () => void;
}) {
  const { drafts, loaded, active, producing, busy, notice, produce, runAction, load } =
    useDraftPanel<CrmSalesIntentDraftWithRevision>({
      itemId,
      produceKind: "crm_sales_intent",
      onUnauthorized,
      fetchDrafts: (id) => api.crmSalesIntentDrafts(id),
      produceDraft: (req) => api.produceCrmSalesIntent(req),
      actionDraft: (draftId, req) => api.crmSalesIntentAction(draftId, req),
      produceTimeoutText:
        "The lead draft didn't finish after 3 minutes — drafting may have failed (check AI Usage). Try again.",
    });

  const [edit, setEdit] = useDraftEdit<CrmSalesIntentDraftWithRevision, SalesIntentEdit>(
    active,
    (entry) => ({
      company_name: entry.draft.company_name ?? "",
      contact_name: entry.draft.contact_name ?? "",
      contact_email: entry.draft.contact_email ?? "",
      lead_title: entry.draft.lead_title,
      intent_summary: entry.draft.intent_summary,
      rationale: entry.draft.rationale,
      qualification_status: entry.draft.qualification_status,
      next_step_text: entry.draft.next_step_text,
      follow_up_due_date: entry.draft.follow_up_due_date ?? "",
      provider_target: entry.draft.provider_target,
      create_businessos_task: entry.draft.create_businessos_task,
    }),
  );

  const dirty =
    active != null &&
    edit != null &&
    (edit.company_name !== (active.draft.company_name ?? "") ||
      edit.contact_name !== (active.draft.contact_name ?? "") ||
      edit.contact_email !== (active.draft.contact_email ?? "") ||
      edit.lead_title !== active.draft.lead_title ||
      edit.intent_summary !== active.draft.intent_summary ||
      edit.rationale !== active.draft.rationale ||
      edit.qualification_status !== active.draft.qualification_status ||
      edit.next_step_text !== active.draft.next_step_text ||
      edit.follow_up_due_date !== (active.draft.follow_up_due_date ?? "") ||
      edit.provider_target !== active.draft.provider_target ||
      edit.create_businessos_task !== active.draft.create_businessos_task);

  const quoteFor = (field: string) =>
    active?.draft.provenance.find((p) => p.field === field)?.quote ?? "";

  return (
    <DraftPanelShell loaded={loaded} notice={notice}>
      {!active ? (
        <DraftEmptyCta
          message="No CRM lead draft yet — draft sales intent from this source, then review before anything is written to the CRM."
          buttonLabel="Draft lead"
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
              <div className="grid gap-2 sm:grid-cols-2">
                <DraftFieldInput
                  label="Company"
                  value={edit.company_name}
                  onChange={(company_name) => setEdit({ ...edit, company_name })}
                  quote={quoteFor("company_name")}
                  placeholder="unknown"
                  disabled={busy}
                />
                <DraftFieldInput
                  label="Contact"
                  value={edit.contact_name}
                  onChange={(contact_name) => setEdit({ ...edit, contact_name })}
                  quote={quoteFor("contact_name")}
                  placeholder="unknown"
                  disabled={busy}
                />
              </div>
              <DraftFieldInput
                label="Contact email"
                value={edit.contact_email}
                onChange={(contact_email) => setEdit({ ...edit, contact_email })}
                quote={quoteFor("contact_email")}
                placeholder="unknown"
                disabled={busy}
              />
              <DraftFieldInput
                label="Lead title"
                value={edit.lead_title}
                onChange={(lead_title) => setEdit({ ...edit, lead_title })}
                quote={quoteFor("lead_title")}
                disabled={busy}
              />
              <DraftFieldInput
                label="Intent"
                value={edit.intent_summary}
                onChange={(intent_summary) => setEdit({ ...edit, intent_summary })}
                quote={quoteFor("intent_summary")}
                multiline
                disabled={busy}
              />
              <DraftFieldInput
                label="Rationale"
                value={edit.rationale}
                onChange={(rationale) => setEdit({ ...edit, rationale })}
                quote={quoteFor("rationale")}
                multiline
                disabled={busy}
              />
              <div className="grid gap-2 sm:grid-cols-2">
                <DraftFieldInput
                  label="Next step"
                  value={edit.next_step_text}
                  onChange={(next_step_text) => setEdit({ ...edit, next_step_text })}
                  quote={quoteFor("next_step_text")}
                  disabled={busy}
                />
                <DraftFieldInput
                  label="Follow-up date"
                  value={edit.follow_up_due_date}
                  onChange={(follow_up_due_date) =>
                    setEdit({ ...edit, follow_up_due_date })
                  }
                  quote={quoteFor("follow_up_due_date")}
                  placeholder="YYYY-MM-DD"
                  disabled={busy}
                />
              </div>
              <div className="grid gap-2 sm:grid-cols-2">
                <label className="flex flex-col gap-1 text-xs text-zinc-400">
                  Qualification
                  <select
                    className="rounded-md border border-zinc-800 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-100"
                    value={edit.qualification_status}
                    onChange={(event) =>
                      setEdit({ ...edit, qualification_status: event.target.value })
                    }
                    disabled={busy}
                  >
                    <option value="qualified">Qualified</option>
                    <option value="unqualified">Unqualified</option>
                    <option value="unknown">Unknown</option>
                  </select>
                </label>
                <label className="flex flex-col gap-1 text-xs text-zinc-400">
                  Provider target
                  <select
                    className="rounded-md border border-zinc-800 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-100"
                    value={edit.provider_target}
                    onChange={(event) =>
                      setEdit({
                        ...edit,
                        provider_target: event.target.value as CrmSalesIntentProviderTarget,
                      })
                    }
                    disabled={busy}
                  >
                    <option value="lead">Lead</option>
                    <option value="deal">Deal</option>
                    <option value="task_only">Task only</option>
                  </select>
                </label>
              </div>
              <label className="flex items-center gap-2 text-xs text-zinc-300">
                <input
                  type="checkbox"
                  checked={edit.create_businessos_task}
                  onChange={(event) =>
                    setEdit({ ...edit, create_businessos_task: event.target.checked })
                  }
                  disabled={busy}
                />
                Create BusinessOS follow-up task when supported
              </label>
            </div>
          ) : (
            <div className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
              {[
                ["Lead", active.draft.lead_title],
                ["Company", active.draft.company_name ?? "—"],
                ["Contact", active.draft.contact_name ?? "—"],
                ["Intent", active.draft.intent_summary],
                ["Next step", active.draft.next_step_text],
                ["Target", active.draft.provider_target],
              ].map(([label, value]) => (
                <div key={label} className="contents">
                  <span className="text-zinc-400">{label}</span>
                  <span className="text-zinc-200">{value}</span>
                </div>
              ))}
            </div>
          )}

          <DraftActionFooter
            visible={active.draft.status === "staged"}
            busy={busy}
            dirty={dirty}
            approveLabel="Approve → CRM lead"
            approveDirtyLabel="Save & approve → CRM lead"
            approveTitle="Creates a CRM Lead when your provider and target support it."
            onApprove={() =>
              void runAction(
                active,
                "approve",
                dirty && edit
                  ? async (revision) => {
                      const saved = await api.updateCrmSalesIntent(
                        active.draft.draft_id,
                        {
                          company_name: nullable(edit.company_name),
                          contact_name: nullable(edit.contact_name),
                          contact_email: nullable(edit.contact_email),
                          lead_title: edit.lead_title,
                          intent_summary: edit.intent_summary,
                          rationale: edit.rationale,
                          qualification_status: edit.qualification_status,
                          next_step_text: edit.next_step_text,
                          follow_up_due_date: nullable(edit.follow_up_due_date),
                          provider_target: edit.provider_target,
                          create_businessos_task: edit.create_businessos_task,
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
                company_name: active.draft.company_name ?? "",
                contact_name: active.draft.contact_name ?? "",
                contact_email: active.draft.contact_email ?? "",
                lead_title: active.draft.lead_title,
                intent_summary: active.draft.intent_summary,
                rationale: active.draft.rationale,
                qualification_status: active.draft.qualification_status,
                next_step_text: active.draft.next_step_text,
                follow_up_due_date: active.draft.follow_up_due_date ?? "",
                provider_target: active.draft.provider_target,
                create_businessos_task: active.draft.create_businessos_task,
              })
            }
          />

          <OutboxStateLine
            job={active.outbox_job}
            show={active.draft.status === "approved"}
            dryRunText="Tested successfully, but live CRM writes are turned off — ask your administrator to enable them."
            deliveredText={(job) =>
              `Lead created in the CRM${job.provider_object_id ? ` (${job.provider_object_id})` : ""}`
            }
            onUnauthorized={onUnauthorized}
            onRetried={load}
          />
        </div>
      )}
    </DraftPanelShell>
  );
}

function nullable(value: string): string | null {
  const trimmed = value.trim();
  return trimmed === "" ? null : trimmed;
}
