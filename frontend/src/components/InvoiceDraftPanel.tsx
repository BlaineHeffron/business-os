import { useCallback, useEffect, useState } from "react";
import type { EnrichmentRun } from "../types/generated/EnrichmentRun";
import type { InvoiceDraftLineItem } from "../types/generated/InvoiceDraftLineItem";
import type { InvoiceDraftWithRevision } from "../types/generated/InvoiceDraftWithRevision";
import { api, errorMessage, isUnauthorized } from "../lib/api";
import { isTerminalEnrichmentStatus } from "../lib/enrichment";
import { Button } from "./ui";
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

type LineEdit = {
  label: string;
  description: string;
  quantity: string; // integer text
  unitAmount: string; // dollars text, converted to cents on save
};

type InvoiceEdit = {
  customer_name: string;
  customer_email: string;
  due_date: string;
  memo: string;
  lines: LineEdit[];
};

function centsToDollars(cents: number): string {
  return (cents / 100).toFixed(2);
}

function dollarsToCents(raw: string): number | null {
  const cleaned = raw.replace(/[$,\s]/g, "");
  if (cleaned === "" || !/^\d+(\.\d{1,2})?$/.test(cleaned)) return null;
  return Math.round(parseFloat(cleaned) * 100);
}

function fmtMoney(cents: number): string {
  return (cents / 100).toLocaleString("en-US", {
    style: "currency",
    currency: "USD",
    minimumFractionDigits: 2,
  });
}

function seedEdit(entry: InvoiceDraftWithRevision): InvoiceEdit {
  return {
    customer_name: entry.draft.customer_name,
    customer_email: entry.draft.customer_email ?? "",
    due_date: entry.draft.due_date ?? "",
    memo: entry.draft.memo,
    lines: entry.draft.line_items.map((line) => ({
      label: line.label,
      description: line.description ?? "",
      quantity: String(line.quantity),
      unitAmount: centsToDollars(line.unit_amount_cents),
    })),
  };
}

/** Edited lines → wire shape (line numbers/totals are server-recomputed).
 * Null when any line is invalid. */
function linesToWire(lines: LineEdit[]): InvoiceDraftLineItem[] | null {
  if (lines.length === 0) return null;
  const wire: InvoiceDraftLineItem[] = [];
  for (const [index, line] of lines.entries()) {
    const quantity = /^\d+$/.test(line.quantity.trim())
      ? parseInt(line.quantity.trim(), 10)
      : NaN;
    const cents = dollarsToCents(line.unitAmount);
    if (
      line.label.trim() === "" ||
      !Number.isFinite(quantity) ||
      quantity < 1 ||
      cents === null ||
      cents <= 0
    ) {
      return null;
    }
    wire.push({
      line_number: index + 1,
      label: line.label.trim(),
      description: line.description.trim() === "" ? null : line.description.trim(),
      quantity,
      unit_amount_cents: cents,
      line_total_cents: quantity * cents, // display only; server recomputes
    });
  }
  return wire;
}

function editTotalCents(lines: LineEdit[]): number | null {
  const wire = linesToWire(lines);
  if (!wire) return null;
  return wire.reduce((sum, line) => sum + line.line_total_cents, 0);
}

/** Detail panel under an accepted queue row for the invoice_draft kind:
 * produce an invoice draft (every line amount quoted from the source;
 * totals recomputed server-side), edit in place, then approve (creates the
 * client + DRAFT invoice in the configured invoicing provider — Invoice
 * Ninja or Stripe — through its write gate; reviewing and sending the
 * invoice stays human in the provider's UI) or reject. */
export default function InvoiceDraftPanel({
  itemId,
  onUnauthorized,
}: {
  itemId: string;
  onUnauthorized: () => void;
}) {
  const {
    drafts,
    loaded,
    active,
    producing,
    notice,
    produce,
    runAction,
    load,
    busy,
    setNotice,
  } = useDraftPanel<InvoiceDraftWithRevision>({
    itemId,
    produceKind: "invoice_draft",
    onUnauthorized,
    fetchDrafts: (id) => api.invoiceDrafts(id),
    produceDraft: (req) => api.produceInvoiceDraft(req),
    actionDraft: (draftId, req) => api.invoiceDraftAction(draftId, req),
    produceTimeoutText:
      "The draft didn't finish after 3 minutes — drafting may have failed (check AI Usage). Try again.",
  });

  const [edit, setEdit] = useDraftEdit<InvoiceDraftWithRevision, InvoiceEdit>(
    active,
    seedEdit,
  );

  const [enrichmentRun, setEnrichmentRun] = useState<EnrichmentRun | null>(null);
  const [pendingEnrichment, setPendingEnrichment] = useState<{
    runId: string;
    alreadyRunning: boolean;
  } | null>(null);
  const [domainSeed, setDomainSeed] = useState("");

  const loadLatestEnrichmentRun = useCallback(async () => {
    if (!active) return null;
    const response = await api.enrichmentRuns({
      sliceId: "invoice_drafts",
      draftId: active.draft.draft_id,
      limit: 1,
    });
    const run = response.runs[0] ?? null;
    setEnrichmentRun(run);
    return run;
  }, [active?.draft.draft_id]);

  useEffect(() => {
    if (!active) {
      setEnrichmentRun(null);
      setPendingEnrichment(null);
      return;
    }
    let cancelled = false;
    void loadLatestEnrichmentRun()
      .then((run) => {
        if (cancelled || !run) return;
        if (pendingEnrichment && isTerminalEnrichmentStatus(run.status)) {
          setPendingEnrichment(null);
        }
      })
      .catch((err: unknown) => {
        if (isUnauthorized(err)) onUnauthorized();
        if (!cancelled) setEnrichmentRun(null);
      });
    return () => {
      cancelled = true;
    };
  }, [active?.draft.draft_id, loadLatestEnrichmentRun, onUnauthorized, pendingEnrichment]);

  useEffect(() => {
    if (!active || !pendingEnrichment) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const response = await api.enrichmentRuns({
          sliceId: "invoice_drafts",
          draftId: active.draft.draft_id,
          limit: 3,
        });
        if (cancelled) return;
        const run =
          response.runs.find((entry) => entry.run_id === pendingEnrichment.runId) ??
          response.runs[0] ??
          null;
        setEnrichmentRun(run);
        if (run && isTerminalEnrichmentStatus(run.status)) {
          setPendingEnrichment(null);
          await load();
        }
      } catch (err) {
        if (isUnauthorized(err)) onUnauthorized();
      }
    };
    void tick();
    const id = setInterval(() => void tick(), 3_000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [active?.draft.draft_id, load, onUnauthorized, pendingEnrichment]);

  const dirty =
    active != null &&
    edit != null &&
    JSON.stringify(edit) !== JSON.stringify(seedEdit(active));

  const runInvoiceAction = async (
    entry: InvoiceDraftWithRevision,
    action: "approve" | "reject",
  ) => {
    if (action === "approve" && edit) {
      if (edit.customer_email.trim() === "") {
        setNotice({
          text: "A customer email is required (it becomes the client record) — add one before approving.",
          kind: "error",
        });
        return;
      }
      if (dirty && !linesToWire(edit.lines)) {
        setNotice({
          text: "Every line needs a label, a whole-number quantity, and a positive unit amount (e.g. 1500.00).",
          kind: "error",
        });
        return;
      }
    }

    await runAction(
      entry,
      action,
      action === "approve" && dirty && edit
        ? async (revision) => {
            const lines = linesToWire(edit.lines);
            if (!lines) {
              throw new Error(
                "Every line needs a label, a whole-number quantity, and a positive unit amount (e.g. 1500.00).",
              );
            }
            const saved = await api.updateInvoiceDraft(entry.draft.draft_id, {
              customer_name: edit.customer_name.trim(),
              customer_email:
                edit.customer_email.trim() === "" ? null : edit.customer_email.trim(),
              due_date: edit.due_date.trim() === "" ? null : edit.due_date.trim(),
              memo: edit.memo.trim(),
              line_items: lines,
              expected_revision: revision,
              idempotency_key: crypto.randomUUID(),
              actor_id: null,
            });
            return saved.revision ?? revision + 1;
          }
        : undefined,
    );
  };

  const runEnrichment = async () => {
    if (!active) return;
    setNotice(null);
    try {
      const response = await api.enrichInvoiceDraft(active.draft.draft_id, {
        idempotency_key: crypto.randomUUID(),
        domain_seed: domainSeed.trim() === "" ? null : domainSeed.trim(),
      });
      setPendingEnrichment({
        runId: response.run_id,
        alreadyRunning: response.already_running,
      });
      setEnrichmentRun(null);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice({ text: `Enrichment failed: ${errorMessage(err)}`, kind: "error" });
    }
  };

  const quoteFor = (field: string) =>
    active?.draft.provenance.find((p) => p.field === field)?.quote ?? "";

  const lineInputCls =
    "rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs text-zinc-200 placeholder:text-zinc-500 focus:border-sky-600 focus:outline-none disabled:opacity-40";

  return (
    <DraftPanelShell loaded={loaded} notice={notice}>
      {!active ? (
        <DraftEmptyCta
          message="No invoice draft yet — draft one from this item with AI (every line amount must be quoted from the source), then review before anything reaches your invoicing system."
          buttonLabel="Draft invoice"
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

          {active.draft.status === "staged" ? (
            <div className="flex flex-wrap items-center gap-2 rounded-md border border-zinc-800 bg-zinc-950 px-2 py-1">
              <input
                value={domainSeed}
                onChange={(event) => setDomainSeed(event.target.value)}
                className="h-7 w-44 rounded-md border border-zinc-700 bg-zinc-950 px-2 text-xs text-zinc-200 placeholder:text-zinc-500 focus:border-sky-600 focus:outline-none disabled:opacity-40"
                placeholder="Domain (optional)"
                disabled={pendingEnrichment != null}
              />
              <Button
                variant="secondary"
                size="sm"
                busy={pendingEnrichment != null}
                onClick={() => void runEnrichment()}
                title="Run customer web enrichment for this staged draft"
              >
                {pendingEnrichment ? "Enriching…" : "Enrich"}
              </Button>
              <span className="min-w-0 flex-1 text-xs text-zinc-400">
                {pendingEnrichment
                  ? `${pendingEnrichment.alreadyRunning ? "Already running" : "Running"} · ${pendingEnrichment.runId}`
                  : enrichmentRun
                    ? `Enrichment · ${enrichmentRun.status} · ${enrichmentRun.diagnostics.length} events · ${enrichmentRun.proposals.length} proposals`
                    : "Enrichment · no run yet"}
              </span>
            </div>
          ) : enrichmentRun ? (
            <div className="rounded-md border border-zinc-800 bg-zinc-950 px-2 py-1 text-xs text-zinc-400">
              Enrichment · {enrichmentRun.status} · {enrichmentRun.diagnostics.length} events ·{" "}
              {enrichmentRun.proposals.length} proposals
            </div>
          ) : null}

          {active.draft.status === "staged" && edit ? (
            <div className="flex max-w-2xl flex-col gap-2">
              <DraftFieldInput
                label="Bill to"
                value={edit.customer_name}
                onChange={(customer_name) => setEdit({ ...edit, customer_name })}
                quote={quoteFor("customer_name")}
                disabled={busy}
              />
              <DraftFieldInput
                label="Email"
                value={edit.customer_email}
                onChange={(customer_email) => setEdit({ ...edit, customer_email })}
                quote={quoteFor("customer_email")}
                placeholder="required before approval (invoicing client record)"
                disabled={busy}
              />
              <DraftFieldInput
                label="Due"
                value={edit.due_date}
                onChange={(due_date) => setEdit({ ...edit, due_date })}
                quote={quoteFor("due_date")}
                placeholder="YYYY-MM-DD, blank = no due date"
                disabled={busy}
              />
              <DraftFieldInput
                label="Memo"
                value={edit.memo}
                onChange={(memo) => setEdit({ ...edit, memo })}
                quote=""
                placeholder="invoice memo"
                disabled={busy}
              />

              <div className="flex flex-col gap-1">
                <span className="text-xs font-medium text-zinc-400">
                  Line items
                </span>
                {edit.lines.map((line, index) => {
                  const quote = quoteFor(`line_${index + 1}_amount`);
                  return (
                    <div key={index} className="flex flex-col gap-0.5">
                      <div className="flex items-center gap-1.5">
                        <input
                          value={line.label}
                          onChange={(e) =>
                            setEdit({
                              ...edit,
                              lines: edit.lines.map((l, i) =>
                                i === index ? { ...l, label: e.target.value } : l,
                              ),
                            })
                          }
                          className={`flex-1 ${lineInputCls}`}
                          placeholder="what's billed"
                          disabled={busy}
                        />
                        <input
                          value={line.quantity}
                          onChange={(e) =>
                            setEdit({
                              ...edit,
                              lines: edit.lines.map((l, i) =>
                                i === index ? { ...l, quantity: e.target.value } : l,
                              ),
                            })
                          }
                          className={`w-14 text-right tabular-nums ${lineInputCls}`}
                          title="Quantity"
                          disabled={busy}
                        />
                        <span className="text-xs text-zinc-500">×</span>
                        <input
                          value={line.unitAmount}
                          onChange={(e) =>
                            setEdit({
                              ...edit,
                              lines: edit.lines.map((l, i) =>
                                i === index ? { ...l, unitAmount: e.target.value } : l,
                              ),
                            })
                          }
                          className={`w-24 text-right tabular-nums ${lineInputCls}`}
                          title="Unit amount ($)"
                          disabled={busy}
                        />
                        <Button
                          variant="ghost"
                          size="sm"
                          disabled={busy || edit.lines.length === 1}
                          onClick={() =>
                            setEdit({
                              ...edit,
                              lines: edit.lines.filter((_, i) => i !== index),
                            })
                          }
                          title="Remove line"
                        >
                          ✕
                        </Button>
                      </div>
                      {quote ? (
                        <span
                          className="pl-1 text-xs italic text-zinc-500"
                          title="Source quote backing this amount"
                        >
                          "{quote}"
                        </span>
                      ) : null}
                    </div>
                  );
                })}
                <div className="flex items-center gap-3">
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={busy || edit.lines.length >= 20}
                    onClick={() =>
                      setEdit({
                        ...edit,
                        lines: [
                          ...edit.lines,
                          { label: "", description: "", quantity: "1", unitAmount: "" },
                        ],
                      })
                    }
                  >
                    + Add line
                  </Button>
                  <span className="text-xs tabular-nums text-zinc-300">
                    Total:{" "}
                    {editTotalCents(edit.lines) !== null
                      ? fmtMoney(editTotalCents(edit.lines) ?? 0)
                      : "—"}
                  </span>
                </div>
              </div>
            </div>
          ) : (
            <div className="flex flex-col gap-1 text-xs">
              <div className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
                {(
                  [
                    ["Bill to", active.draft.customer_name, quoteFor("customer_name")],
                    ["Email", active.draft.customer_email ?? "—", quoteFor("customer_email")],
                    ["Due", active.draft.due_date ?? "—", quoteFor("due_date")],
                    ["Memo", active.draft.memo || "—", ""],
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
              <div className="mt-1 flex flex-col gap-0.5">
                {active.draft.line_items.map((line) => (
                  <div key={line.line_number} className="flex justify-between gap-4">
                    <span className="text-zinc-300">
                      {line.label}
                      {line.description ? (
                        <span className="text-zinc-500"> — {line.description}</span>
                      ) : null}
                    </span>
                    <span className="tabular-nums text-zinc-200">
                      {line.quantity} × {fmtMoney(line.unit_amount_cents)} ={" "}
                      {fmtMoney(line.line_total_cents)}
                    </span>
                  </div>
                ))}
                <div className="flex justify-between gap-4 border-t border-zinc-800 pt-0.5 font-semibold">
                  <span className="text-zinc-300">Total</span>
                  <span className="tabular-nums text-zinc-100">
                    {fmtMoney(active.draft.total_cents)}
                  </span>
                </div>
              </div>
            </div>
          )}

          <DraftActionFooter
            visible={active.draft.status === "staged"}
            busy={busy}
            dirty={dirty}
            approveLabel="Approve → draft invoice"
            approveDirtyLabel="Save & approve → draft invoice"
            approveTitle="Creates a draft invoice in your invoicing system when you approve — reviewing and sending stays in the provider's UI."
            onApprove={() => void runInvoiceAction(active, "approve")}
            onReject={() => void runInvoiceAction(active, "reject")}
            onResetEdits={() => setEdit(seedEdit(active))}
          />

          <OutboxStateLine
            job={active.outbox_job}
            show={active.draft.status === "approved"}
            dryRunText="Tested successfully, but live invoice creation is turned off — ask your administrator to enable it."
            deliveredText={(job) =>
              `Draft invoice created${job.provider_object_id ? ` (${job.provider_object_id})` : ""} — review and send it from your invoicing system`
            }
            onUnauthorized={onUnauthorized}
            onRetried={load}
          />
        </div>
      )}
    </DraftPanelShell>
  );
}
