import { Fragment } from "react";
import type { ClaimDraftWithRevision } from "../types/generated/ClaimDraftWithRevision";
import type { ClaimShipmentRefs } from "../types/generated/ClaimShipmentRefs";
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

type ClaimEdit = {
  damage_narrative: string;
  item_description: string;
  claim_amount: string;
};

/** Detail panel under an accepted queue row for the claim_draft kind: a
 * provider-neutral shipping damage packet assembled from cached evidence.
 * Shipment, order, and evidence fields are cached provider truth (not
 * editable); the narrative, item description, and claim amount are. */
export default function ClaimDraftPanel({
  itemId,
  onUnauthorized,
}: {
  itemId: string;
  onUnauthorized: () => void;
}) {
  const { drafts, loaded, active, producing, busy, notice, produce, runAction, load } =
    useDraftPanel<ClaimDraftWithRevision>({
      itemId,
      produceKind: "claim_draft",
      onUnauthorized,
      fetchDrafts: (id) => api.claimDrafts(id),
      produceDraft: (req) => api.produceClaimDraft(req),
      actionDraft: (draftId, req) => api.claimDraftAction(draftId, req),
      produceTimeoutText:
        "The draft didn't finish after 3 minutes — drafting may have failed (check AI Usage). Try again.",
    });

  const [edit, setEdit] = useDraftEdit<ClaimDraftWithRevision, ClaimEdit>(
    active,
    (entry) => ({
      damage_narrative: entry.draft.damage_narrative,
      item_description: entry.draft.item_description,
      claim_amount: (entry.draft.claim_amount_cents / 100).toFixed(2),
    }),
  );

  const editedCents = edit
    ? Math.round(Number.parseFloat(edit.claim_amount || "0") * 100)
    : 0;
  const dirty =
    active != null &&
    edit != null &&
    (edit.damage_narrative !== active.draft.damage_narrative ||
      edit.item_description !== active.draft.item_description ||
      (Number.isFinite(editedCents) &&
        editedCents !== active.draft.claim_amount_cents));

  const packet = active?.draft.packet;
  const roleLabel = (role: string) =>
    ({
      order_reference: "order reference",
      packing_proof: "pack-time photos",
      tracking_reference: "shipment reference",
      damage_photo: "damage photos",
    })[role] ?? role;

  return (
    <DraftPanelShell loaded={loaded} notice={notice}>
      {!active ? (
        <DraftEmptyCta
          message="No claim packet yet — assemble one from cached shipment evidence (order, pack photos, tracking, damage report)."
          buttonLabel="Assemble claim packet"
          busyLabel="Assembling…"
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

          {packet ? (
            packet.ready ? (
              <div className="rounded-md border border-emerald-900/60 bg-emerald-950/30 px-3 py-1.5 text-xs text-emerald-300">
                Packet complete — order reference, packing proof, shipment reference, and damage photos are all on file.
              </div>
            ) : (
              <div className="rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-1.5 text-xs text-amber-300">
                Packet incomplete — approval is blocked. Missing:{" "}
                {packet.missing_roles.map(roleLabel).join(", ")}. Fix the evidence in
                the source system, then reject and re-assemble.
              </div>
            )
          ) : null}

          <div className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
            <span className="text-zinc-400">Shipment</span>
            <span className="font-mono text-zinc-200">
              {shipmentReferenceLabel(
                active.draft.tracking_number,
                active.draft.shipment_number,
                active.draft.shipment_refs,
              )}
              <span className="ml-2 text-xs text-zinc-500">
                {active.draft.carrier ?? ""} · from {active.draft.shipment_context_source ?? "source"} — not editable
              </span>
            </span>
            {shipmentRefLines(active.draft.shipment_refs).map(([label, value], index) => (
              <Fragment key={`${label}:${value}:${index}`}>
                <span className="text-zinc-400">
                  {label}
                </span>
                <span className="break-all font-mono text-zinc-200">
                  {value}
                </span>
              </Fragment>
            ))}
            <span className="text-zinc-400">Order</span>
            <span className="text-zinc-200">
              {active.draft.order_number ?? "(unmatched)"}
              {active.draft.order_platform ? ` · ${active.draft.order_platform}` : ""}
              {active.draft.external_order_id ? ` ${active.draft.external_order_id}` : ""}
              {active.draft.customer_name ? ` — ${active.draft.customer_name}` : ""}
              {active.draft.order_total_cents != null
                ? ` · $${(active.draft.order_total_cents / 100).toFixed(2)}`
                : ""}
              {active.draft.ship_date ? ` · shipped ${active.draft.ship_date}` : ""}
            </span>
            <span className="text-zinc-400">Damage</span>
            <span className="text-zinc-200">
              {active.draft.damage_type} (severity {active.draft.damage_severity}
              {active.draft.damage_reported_at
                ? `, reported ${active.draft.damage_reported_at.slice(0, 10)}`
                : ""}
              )
            </span>
          </div>

          {active.draft.status === "staged" && edit != null ? (
            <div className="flex max-w-2xl flex-col gap-2">
              <DraftFieldInput
                label="Narrative"
                value={edit.damage_narrative}
                onChange={(damage_narrative) => setEdit({ ...edit, damage_narrative })}
                quote=""
                multiline
                disabled={busy}
              />
              <div className="grid grid-cols-2 gap-2">
                <DraftFieldInput
                  label="Item(s)"
                  value={edit.item_description}
                  onChange={(item_description) => setEdit({ ...edit, item_description })}
                  quote=""
                  disabled={busy}
                />
                <DraftFieldInput
                  label="Claim amount ($)"
                  value={edit.claim_amount}
                  onChange={(claim_amount) => setEdit({ ...edit, claim_amount })}
                  quote=""
                  disabled={busy}
                />
              </div>
            </div>
          ) : (
            <div className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
              <span className="text-zinc-400">Narrative</span>
              <span className="whitespace-pre-wrap text-zinc-200">
                {active.draft.damage_narrative}
              </span>
              <span className="text-zinc-400">Amount</span>
              <span className="text-zinc-200">
                ${(active.draft.claim_amount_cents / 100).toFixed(2)}
              </span>
            </div>
          )}

          <div className="flex flex-col gap-1">
            <span className="text-xs font-semibold uppercase tracking-wide text-zinc-400">
              Evidence
            </span>
            {active.draft.evidence.damage_photo_urls.map((url) => (
              <a
                key={url}
                href={url}
                target="_blank"
                rel="noreferrer"
                className="truncate text-xs text-sky-400 hover:underline"
              >
                damage photo — {url}
              </a>
            ))}
            {active.draft.evidence.pack_photo_urls.map((url) => (
              <a
                key={url}
                href={url}
                target="_blank"
                rel="noreferrer"
                className="truncate text-xs text-sky-400 hover:underline"
              >
                pack photo — {url}
              </a>
            ))}
            {active.draft.evidence.pack_photo_urls.length === 0 ? (
              <span className="text-xs text-zinc-500">
                {active.draft.evidence.pack_photo_count > 0
                  ? `${active.draft.evidence.pack_photo_count} pack photo(s) in the source system (open the order's pack record)`
                  : "no pack photos on file"}
              </span>
            ) : null}
          </div>

          <DraftActionFooter
            visible={active.draft.status === "staged"}
            busy={busy}
            dirty={dirty}
            approveLabel="Approve → Gmail draft"
            approveDirtyLabel="Save & approve → Gmail draft"
            approveDisabled={busy || !packet?.ready}
            approveTitle={
              packet?.ready
                ? "Stages a Gmail draft with the packet + evidence links to the filing mailbox; provider filing stays with you (a tracking task is created)"
                : "Blocked: the packet is missing required evidence"
            }
            onApprove={() =>
              void runAction(
                active,
                "approve",
                edit != null && dirty
                  ? async (revision) => {
                      const saved = await api.updateClaimDraft(
                        active.draft.draft_id,
                        {
                          damage_narrative: edit.damage_narrative,
                          item_description: edit.item_description,
                          claim_amount_cents: editedCents,
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
          />

          <OutboxStateLine
            job={active.outbox_job}
            show={active.draft.status === "approved"}
            dryRunText="Tested successfully, but live Gmail drafts are turned off — ask your administrator to enable them."
            deliveredText={() =>
              "Packet draft created in Gmail — download the evidence and file in the provider workflow; the tracking task is on the Tasks tab"
            }
            onUnauthorized={onUnauthorized}
            onRetried={load}
          />
        </div>
      )}
    </DraftPanelShell>
  );
}

function shipmentReferenceLabel(
  trackingNumber?: string | null,
  shipmentNumber?: string | null,
  refs?: ClaimShipmentRefs | null,
) {
  if (refs?.pro_number) return `PRO ${refs.pro_number}`;
  if (refs?.bol_number) return `BOL ${refs.bol_number}`;
  if (refs?.tracking_number) return refs.tracking_number;
  if (refs?.platform_shipment_id) {
    return `${refs.shipping_platform ?? "platform"} shipment ${refs.platform_shipment_id}`;
  }
  return trackingNumber ?? shipmentNumber ?? "(unknown shipment)";
}

function shipmentRefLines(refs?: ClaimShipmentRefs | null) {
  if (!refs) return [];
  const rows: Array<[string, string]> = [];
  const push = (label: string, value?: string | null) => {
    if (value) rows.push([label, value]);
  };
  push("Platform", refs.shipping_platform);
  push("Platform ID", refs.platform_shipment_id);
  push("Service", refs.carrier_service);
  push("Mode", refs.mode);
  push("Tracking", refs.tracking_number);
  push("PRO", refs.pro_number);
  push("BOL", refs.bol_number);
  push("Tracking URL", refs.tracking_url);
  push("Claim platform", refs.claim_platform);
  refs.document_refs.forEach((doc) => push(`Doc ${doc.kind}`, doc.url));
  return rows;
}
