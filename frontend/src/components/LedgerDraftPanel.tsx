import type { LedgerDraftWithRevision } from "../types/generated/LedgerDraftWithRevision";
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

type ReceiptEdit = {
  payer_name: string;
  payer_email: string;
  amount: string; // dollars, e.g. "1500.00" — converted to cents on save
  paid_date: string;
  description: string;
};

function centsToDollars(cents: number): string {
  return (cents / 100).toFixed(2);
}

function dollarsToCents(raw: string): number | null {
  const cleaned = raw.replace(/[$,\s]/g, "");
  if (cleaned === "" || !/^\d+(\.\d{1,2})?$/.test(cleaned)) return null;
  return Math.round(parseFloat(cleaned) * 100);
}

function seedEdit(entry: LedgerDraftWithRevision): ReceiptEdit {
  return {
    payer_name: entry.draft.payer_name,
    payer_email: entry.draft.payer_email ?? "",
    amount: centsToDollars(entry.draft.amount_cents),
    paid_date: entry.draft.paid_date,
    description: entry.draft.description,
  };
}

/** Detail panel under an accepted queue row for the ledger_entry kind:
 * produce the received-payment draft (amount grounded with a literal source
 * quote), edit in place, then approve (records client + invoice + payment in
 * the accounting system — dry-run while the write gate is closed) or
 * reject. */
export default function LedgerDraftPanel({
  itemId,
  onUnauthorized,
}: {
  itemId: string;
  onUnauthorized: () => void;
}) {
  const { drafts, loaded, active, producing, busy, notice, setNotice, produce, runAction, load } =
    useDraftPanel<LedgerDraftWithRevision>({
      itemId,
      produceKind: "ledger_entry",
      onUnauthorized,
      fetchDrafts: (id) => api.ledgerDrafts(id),
      produceDraft: (req) => api.produceLedgerDraft(req),
      actionDraft: (draftId, req) => api.ledgerDraftAction(draftId, req),
      produceTimeoutText:
        "The draft didn't finish after 3 minutes — drafting may have failed (check AI Usage). Try again.",
    });

  const [edit, setEdit] = useDraftEdit<LedgerDraftWithRevision, ReceiptEdit>(
    active,
    seedEdit,
  );

  const dirty =
    active != null &&
    edit != null &&
    (edit.payer_name !== active.draft.payer_name ||
      edit.payer_email !== (active.draft.payer_email ?? "") ||
      edit.amount !== centsToDollars(active.draft.amount_cents) ||
      edit.paid_date !== active.draft.paid_date ||
      edit.description !== active.draft.description);

  const quoteFor = (field: string) =>
    active?.draft.provenance.find((p) => p.field === field)?.quote ?? "";

  return (
    <DraftPanelShell loaded={loaded} notice={notice}>
      {!active ? (
        <DraftEmptyCta
          message="No payment record yet — draft one from this message with AI (the amount must be quoted from the source), then review before anything is written to accounting."
          buttonLabel="Draft payment record"
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
                label="Payer"
                value={edit.payer_name}
                onChange={(payer_name) => setEdit({ ...edit, payer_name })}
                quote={quoteFor("payer_name")}
                disabled={busy}
              />
              <DraftFieldInput
                label="Email"
                value={edit.payer_email}
                onChange={(payer_email) => setEdit({ ...edit, payer_email })}
                quote={quoteFor("payer_email")}
                placeholder="no payer email"
                disabled={busy}
              />
              <DraftFieldInput
                label="Amount ($)"
                value={edit.amount}
                onChange={(amount) => setEdit({ ...edit, amount })}
                quote={quoteFor("amount_cents")}
                disabled={busy}
              />
              <DraftFieldInput
                label="Paid on"
                value={edit.paid_date}
                onChange={(paid_date) => setEdit({ ...edit, paid_date })}
                quote={quoteFor("paid_date")}
                placeholder="YYYY-MM-DD"
                disabled={busy}
              />
              <DraftFieldInput
                label="For"
                value={edit.description}
                onChange={(description) => setEdit({ ...edit, description })}
                quote=""
                placeholder="what the payment was for"
                disabled={busy}
              />
            </div>
          ) : (
            <div className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
              {(
                [
                  ["Payer", active.draft.payer_name, quoteFor("payer_name")],
                  [
                    "Email",
                    active.draft.payer_email ?? "—",
                    quoteFor("payer_email"),
                  ],
                  [
                    "Amount",
                    `$${centsToDollars(active.draft.amount_cents)}`,
                    quoteFor("amount_cents"),
                  ],
                  ["Paid on", active.draft.paid_date, quoteFor("paid_date")],
                  ["For", active.draft.description || "—", ""],
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
            approveLabel="Approve → books"
            approveDirtyLabel="Save & approve → books"
            approveTitle="Records the payment in your accounting system when you approve."
            onApprove={() => {
              // Amount guard: abort before any network call if the cents
              // conversion is invalid — mirrors the original pre-check ordering.
              if (dirty && edit) {
                const cents = dollarsToCents(edit.amount);
                if (cents === null || cents <= 0) {
                  setNotice({
                    text: "Amount must be a positive dollar value (e.g. 1500.00).",
                    kind: "error",
                  });
                  return;
                }
              }
              void runAction(
                active,
                "approve",
                dirty && edit
                  ? async (revision) => {
                      const saved = await api.updateLedgerDraft(
                        active.draft.draft_id,
                        {
                          payer_name: edit.payer_name.trim(),
                          payer_email:
                            edit.payer_email.trim() === ""
                              ? null
                              : edit.payer_email.trim(),
                          amount_cents: dollarsToCents(edit.amount)!,
                          paid_date: edit.paid_date.trim(),
                          description: edit.description.trim(),
                          expected_revision: revision,
                          idempotency_key: crypto.randomUUID(),
                          actor_id: null,
                        },
                      );
                      return saved.revision ?? revision + 1;
                    }
                  : undefined,
              );
            }}
            onReject={() => void runAction(active, "reject")}
            onResetEdits={() => setEdit(seedEdit(active))}
          />

          <OutboxStateLine
            job={active.outbox_job}
            show={active.draft.status === "approved"}
            dryRunText="Tested successfully, but live payment recording is turned off — ask your administrator to enable it."
            deliveredText={(job) =>
              `Payment recorded in accounting${job.provider_object_id ? ` (invoice ${job.provider_object_id})` : ""}`
            }
            onUnauthorized={onUnauthorized}
            onRetried={load}
          />
        </div>
      )}
    </DraftPanelShell>
  );
}
