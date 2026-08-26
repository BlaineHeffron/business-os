import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ComponentType,
  type ReactNode,
} from "react";
import type { AttentionLevel } from "../types/generated/AttentionLevel";
import type { LaunchAgentResponse } from "../types/generated/LaunchAgentResponse";
import type { PacketKindRecord } from "../types/generated/PacketKindRecord";
import type { TaskEscalation } from "../types/generated/TaskEscalation";
import type { WorkItemActionKind } from "../types/generated/WorkItemActionKind";
import type { WorkItemWithRevision } from "../types/generated/WorkItemWithRevision";
import {
  api,
  errorMessage,
  isRevisionConflict,
  isUnauthorized,
} from "../lib/api";
import { useAppCommand } from "../lib/commands";
import { useCategories } from "../lib/categories";
import { usePolling } from "../lib/usePolling";
import CategoryBadge from "../components/CategoryBadge";
import CalendarDraftPanel from "../components/CalendarDraftPanel";
import ClaimDraftPanel from "../components/ClaimDraftPanel";
import ContentDraftPanel from "../components/ContentDraftPanel";
import CrmDraftPanel from "../components/CrmDraftPanel";
import CrmRecordDraftPanel from "../components/CrmRecordDraftPanel";
import CrmSalesIntentPanel from "../components/CrmSalesIntentPanel";
import LedgerDraftPanel from "../components/LedgerDraftPanel";
import InvoiceDraftPanel from "../components/InvoiceDraftPanel";
import EmailDraftPanel from "../components/EmailDraftPanel";
import FollowUpDraftPanel from "../components/FollowUpDraftPanel";
import SectionHelpButton from "../components/SectionHelpButton";
import SourcePeek from "../components/SourcePeek";
import { usePacketKinds } from "../lib/packetKinds";
import { localToday } from "./Tasks";
import { Button, EmptyState, StatusBadge } from "../components/ui";
import OutputComposerShell from "../components/output/OutputComposerShell";

const POLL_INTERVAL_MS = 30_000;
/// While the auto-produce pump is drafting something visible.
const FAST_POLL_INTERVAL_MS = 10_000;
const AGENT_WORK_DIR_FALLBACK = "/home/example/projects/BusinessOS";

/** Kinds with a produce flow behind them (drafts panel exists). */
const PRODUCE_KINDS = new Set([
  "calendar_event_draft",
  "follow_up_task",
  "crm_activity",
  "crm_record_create",
  "crm_sales_intent",
  "ledger_entry",
  "invoice_draft",
  "email_draft_reply",
  "content_draft",
  "claim_draft",
]);

// The +Log note form's action checkboxes (D2). Each maps to a packet kind the
// server validates against its catalog; CRM note + records are pre-checked.
const NOTE_ACTIONS: { kind: string; label: string }[] = [
  { kind: "calendar_event_draft", label: "Calendar" },
  { kind: "crm_activity", label: "CRM note" },
  { kind: "crm_record_create", label: "CRM records" },
  { kind: "crm_sales_intent", label: "CRM lead" },
  { kind: "invoice_draft", label: "Invoice" },
  { kind: "follow_up_task", label: "Follow-up" },
];

// The draft panels an accepted item can show, one per produced kind. Rendered
// as tabs (a work item often spawns several drafts — e.g. a CRM note + an
// invoice + the contact/company records — and stacking them was visually
// ambiguous). Order = the order tabs appear.
type DraftPanelProps = { itemId: string; onUnauthorized: () => void };
const DRAFT_PANELS: { kind: string; label: string; Panel: ComponentType<DraftPanelProps> }[] = [
  { kind: "calendar_event_draft", label: "Calendar", Panel: CalendarDraftPanel },
  { kind: "follow_up_task", label: "Follow-up", Panel: FollowUpDraftPanel },
  { kind: "ledger_entry", label: "Ledger", Panel: LedgerDraftPanel },
  { kind: "invoice_draft", label: "Invoice", Panel: InvoiceDraftPanel },
  { kind: "crm_activity", label: "CRM note", Panel: CrmDraftPanel },
  { kind: "crm_record_create", label: "CRM records", Panel: CrmRecordDraftPanel },
  { kind: "crm_sales_intent", label: "CRM lead", Panel: CrmSalesIntentPanel },
  { kind: "email_draft_reply", label: "Email", Panel: EmailDraftPanel },
  { kind: "content_draft", label: "Content", Panel: ContentDraftPanel },
  { kind: "claim_draft", label: "Claim", Panel: ClaimDraftPanel },
];

function DraftWorkspaceOverlay({
  title,
  itemId,
  tabs,
  activeKind,
  onSelectKind,
  onClose,
  onUnauthorized,
  children,
}: {
  title: string;
  itemId: string;
  tabs: { kind: string; label: string }[];
  activeKind: string;
  onSelectKind: (kind: string) => void;
  onClose: () => void;
  onUnauthorized: () => void;
  children: ReactNode;
}) {
  return (
    <OutputComposerShell
      title={title}
      mode="queue"
      tabs={tabs.map((tab) => ({ id: tab.kind, label: tab.label }))}
      activeTab={activeKind}
      onSelectTab={onSelectKind}
      contextTitle="Governed source"
      context={<SourcePeek itemId={itemId} onUnauthorized={onUnauthorized} />}
      onClose={onClose}
    >
      {children}
    </OutputComposerShell>
  );
}

const OUTPUT_LABELS = new Map<string, string>([
  ["crm_sales_intent", "CRM lead"],
  ["calendar_event_draft", "Calendar event"],
  ["follow_up_task", "Follow-up task"],
  ["email_draft_reply", "Email draft"],
  ["invoice_draft", "Invoice draft"],
  ["crm_activity", "CRM note"],
  ["crm_record_create", "CRM records"],
  ["content_draft", "Content"],
  ["claim_draft", "Claim"],
  ["ledger_entry", "Ledger"],
]);

const OUTPUT_ORDER = [
  "crm_sales_intent",
  "calendar_event_draft",
  "follow_up_task",
  "email_draft_reply",
  "invoice_draft",
  "crm_activity",
  "crm_record_create",
  "content_draft",
  "claim_draft",
  "ledger_entry",
];

function outputLabel(
  kind: string,
  catalog: Map<string, PacketKindRecord>,
): string {
  return OUTPUT_LABELS.get(kind) ?? catalog.get(kind)?.title ?? kind;
}

async function produceDraftForKind(itemId: string, kind: string): Promise<void> {
  const request = {
    item_id: itemId,
    idempotency_key: crypto.randomUUID(),
    actor_id: null,
  };
  switch (kind) {
    case "calendar_event_draft":
      await api.produceCalendarDraft(request);
      return;
    case "follow_up_task":
      await api.produceFollowUpDraft(request);
      return;
    case "ledger_entry":
      await api.produceLedgerDraft(request);
      return;
    case "invoice_draft":
      await api.produceInvoiceDraft(request);
      return;
    case "crm_activity":
      await api.produceCrmDraft(request);
      return;
    case "crm_record_create":
      await api.produceCrmRecordDraft(request);
      return;
    case "crm_sales_intent":
      await api.produceCrmSalesIntent(request);
      return;
    case "email_draft_reply":
      await api.produceEmailDraft(request);
      return;
    case "content_draft":
      await api.produceContentDraft(request);
      return;
    case "claim_draft":
      await api.produceClaimDraft(request);
      return;
    default:
      return;
  }
}

/** Tabbed draft panels for one accepted item — one tab per produced kind, so
 * the invoice / CRM note / records drafts read as distinct items instead of a
 * stacked wall. A single draft renders without a tab bar. */
function ItemDraftTabs({
  itemId,
  kinds,
  stagedKinds,
  pendingKinds,
  onUnauthorized,
}: {
  itemId: string;
  kinds: string[];
  stagedKinds: string[];
  pendingKinds: string[];
  onUnauthorized: () => void;
}) {
  const tabs = DRAFT_PANELS.filter((p) => kinds.includes(p.kind));
  const [active, setActive] = useState(tabs[0]?.kind ?? "");
  const [editorOpen, setEditorOpen] = useState(false);
  if (tabs.length === 0) return null;
  const activeKind = tabs.some((t) => t.kind === active) ? active : tabs[0].kind;
  const Active = (tabs.find((t) => t.kind === activeKind) ?? tabs[0]).Panel;
  const activeLabel =
    tabs.find((t) => t.kind === activeKind)?.label.toLowerCase() ?? "draft";
  const activeTitle =
    tabs.find((t) => t.kind === activeKind)?.label ?? "Packet editor";
  const queueProducing =
    pendingKinds.includes(activeKind) && !stagedKinds.includes(activeKind);
  return (
    <div className="flex flex-col gap-2 border-t border-zinc-800/90 bg-zinc-950/50 px-4 py-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        {tabs.length > 1 ? (
          <div role="tablist" className="flex flex-wrap gap-1">
            {tabs.map((t) => (
              <button
                key={t.kind}
                role="tab"
                aria-selected={t.kind === activeKind}
                onClick={() => setActive(t.kind)}
                className={`rounded-md px-2.5 py-1 text-xs font-medium ${
                  t.kind === activeKind
                    ? `${outputChipCls(t.kind)} border`
                    : "border border-transparent text-zinc-400 hover:border-zinc-700 hover:bg-zinc-800/50 hover:text-zinc-200"
                }`}
              >
                {t.label}
              </button>
            ))}
          </div>
        ) : (
          <div className="text-xs font-semibold text-zinc-300">
            {activeTitle}
          </div>
        )}
        <Button
          variant="secondary"
          size="sm"
          disabled={queueProducing}
          onClick={() => setEditorOpen(true)}
          title="Open a larger packet editor beside the source"
        >
          Open editor
        </Button>
      </div>
      {editorOpen && !queueProducing ? (
        <DraftWorkspaceOverlay
          title={`${activeTitle} editor`}
          itemId={itemId}
          tabs={tabs.map(({ kind, label }) => ({ kind, label }))}
          activeKind={activeKind}
          onSelectKind={setActive}
          onClose={() => setEditorOpen(false)}
          onUnauthorized={onUnauthorized}
        >
          <Active itemId={itemId} onUnauthorized={onUnauthorized} />
        </DraftWorkspaceOverlay>
      ) : null}
      {editorOpen ? (
        <div className="rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2 text-xs text-zinc-400">
          Editing in the expanded workspace.
        </div>
      ) : queueProducing ? (
        <div className="rounded-md border border-sky-500/20 bg-sky-500/10 px-3 py-2 text-xs text-sky-300">
          Drafting {activeLabel}… It will appear here when ready.
        </div>
      ) : (
        <Active itemId={itemId} onUnauthorized={onUnauthorized} />
      )}
    </div>
  );
}

function formatBytes(bytes: number | null): string {
  if (bytes === null) return "unknown size";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

// "Needs you" is the decision lane: open items plus accepted items whose
// staged drafts await an approve/reject. Everything else is archive views.
type Filter =
  | "needs_you"
  | "attention"
  | "unassigned"
  | "accepted"
  | "dismissed"
  | "all";
type SortMode = "newest" | "status";

const FILTERS: { id: Filter; label: string }[] = [
  { id: "needs_you", label: "Needs you" },
  { id: "attention", label: "Attention" },
  { id: "unassigned", label: "Unassigned" },
  { id: "accepted", label: "Accepted" },
  { id: "dismissed", label: "Dismissed" },
  { id: "all", label: "All" },
];

const FILTER_ACTIVE_CLS: Record<Filter, string> = {
  needs_you: "bg-sky-500/15 text-sky-300 ring-1 ring-inset ring-sky-500/40",
  attention:
    "bg-amber-500/15 text-amber-300 ring-1 ring-inset ring-amber-500/40",
  unassigned: "bg-teal-500/15 text-teal-300 ring-1 ring-inset ring-teal-500/40",
  accepted:
    "bg-emerald-500/15 text-emerald-300 ring-1 ring-inset ring-emerald-500/40",
  dismissed: "bg-zinc-500/15 text-zinc-200 ring-1 ring-inset ring-zinc-500/40",
  all: "bg-zinc-500/15 text-zinc-200 ring-1 ring-inset ring-zinc-500/40",
};

type Notice = { text: string; kind: "error" | "conflict" } | null;

function statusRank(entry: WorkItemWithRevision): number {
  if (entry.item.status === "accepted") {
    if (entry.failure_notifications.length > 0) return 0;
    if (entry.staged_draft_kinds.length > 0) return 1;
    if (entry.pending_produce_kinds.length > 0) return 2;
    return 4;
  }
  if (entry.item.status === "open") return 3;
  if (entry.item.status === "dismissed") return 5;
  return 6;
}

function attentionRank(entry: WorkItemWithRevision): number {
  switch (entry.attention?.level) {
    case "higher":
      return 0;
    case "normal":
      return 1;
    case "lower":
      return 3;
    default:
      return 2;
  }
}

function draftKindsFor(entry: WorkItemWithRevision): string[] {
  const seen = new Set<string>();
  const kinds: string[] = [];
  for (const kind of [
    ...entry.item.packet_kinds,
    ...entry.staged_draft_kinds,
    ...entry.pending_produce_kinds,
  ]) {
    if (!PRODUCE_KINDS.has(kind) || seen.has(kind)) continue;
    seen.add(kind);
    kinds.push(kind);
  }
  return kinds;
}

function sharedVisible(entry: WorkItemWithRevision): boolean {
  const visible = entry.item.visible_to_user_ids;
  if (visible.length > 1) return true;
  return visible.some((userId) => userId !== entry.item.source_user_id);
}

function assignedOrPersonalMine(
  entry: WorkItemWithRevision,
  meId: string | null,
): boolean {
  if (meId === null || meId === "operator") return false;
  if (entry.item.assignee_user_id === meId) return true;
  return (
    entry.item.assignee_user_id === null &&
    !sharedVisible(entry) &&
    entry.item.source_user_id === meId
  );
}

function matchesFilter(entry: WorkItemWithRevision, filter: Filter): boolean {
  switch (filter) {
    case "all":
      return true;
    case "attention":
      return entry.attention?.level === "higher";
    case "unassigned":
      return sharedVisible(entry) && entry.item.assignee_user_id === null;
    case "needs_you":
      // Open decisions, plus accepted items with drafts awaiting
      // approve/reject or production in flight. Dismissed rows stay archived
      // even if an old staged draft still exists.
      return (
        entry.item.status === "open" ||
        (entry.item.status === "accepted" &&
          (entry.staged_draft_kinds.length > 0 ||
            entry.pending_produce_kinds.length > 0 ||
            entry.failure_notifications.length > 0))
      );
    default:
      return entry.item.status === filter;
  }
}

function attentionTone(level: AttentionLevel) {
  if (level === "higher") return "warning";
  if (level === "normal") return "info";
  return "neutral";
}

function attentionTitle(entry: WorkItemWithRevision): string | undefined {
  if (!entry.attention) return undefined;
  const label = attentionDisplayLabel(entry.attention.label);
  const detail = entry.attention.detail?.trim();
  if (!detail) return label;
  return `${label}: ${detail}`;
}

function attentionDisplayLabel(label: string): string {
  const display = label
    .replace(/\btone\b/gi, "")
    .replace(/\s+/g, " ")
    .trim();
  return display || "Attention";
}

function rowRailCls(entry: WorkItemWithRevision): string {
  if (entry.item.status === "accepted") {
    if (entry.failure_notifications.length > 0) return "bg-red-500";
    if (entry.staged_draft_kinds.length > 0) return "bg-amber-400";
    if (entry.pending_produce_kinds.length > 0) return "animate-pulse bg-sky-400";
    return "bg-emerald-500/70";
  }
  if (entry.item.status === "open") {
    return entry.attention?.level === "higher"
      ? "bg-amber-500"
      : "bg-sky-500/80";
  }
  return "bg-zinc-700";
}

function outputChipCls(kind: string): string {
  switch (kind) {
    case "calendar_event_draft":
      return "border-sky-500/30 bg-sky-500/10 text-sky-300";
    case "follow_up_task":
      return "border-amber-500/30 bg-amber-500/10 text-amber-300";
    case "crm_activity":
    case "crm_record_create":
    case "crm_sales_intent":
      return "border-violet-500/30 bg-violet-500/10 text-violet-300";
    case "ledger_entry":
    case "invoice_draft":
      return "border-emerald-500/30 bg-emerald-500/10 text-emerald-300";
    case "email_draft_reply":
      return "border-cyan-500/30 bg-cyan-500/10 text-cyan-300";
    case "content_draft":
      return "border-fuchsia-500/30 bg-fuchsia-500/10 text-fuchsia-300";
    case "claim_draft":
      return "border-red-500/30 bg-red-500/10 text-red-300";
    default:
      return "border-zinc-700 bg-zinc-800/70 text-zinc-300";
  }
}

function relativeAge(ms: number): string {
  const delta = Date.now() - ms;
  if (delta < 0) return "now";
  const s = Math.floor(delta / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  const d = Math.floor(h / 24);
  if (d < 30) return `${d}d`;
  return new Date(ms).toLocaleDateString();
}

const MAX_INLINE_OUTPUTS = 2;

function OutputsPopover({
  kinds,
  catalog,
  onSave,
  onClose,
}: {
  kinds: string[];
  catalog: Map<string, PacketKindRecord>;
  onSave: (kinds: string[]) => void;
  onClose: () => void;
}) {
  const panelRef = useRef<HTMLDivElement>(null);
  const [draftKinds, setDraftKinds] = useState(kinds);
  const options = [...catalog.values()].sort((a, b) => {
    const aIdx = OUTPUT_ORDER.indexOf(a.kind_id);
    const bIdx = OUTPUT_ORDER.indexOf(b.kind_id);
    if (aIdx >= 0 && bIdx >= 0) return aIdx - bIdx;
    if (aIdx >= 0) return -1;
    if (bIdx >= 0) return 1;
    return outputLabel(a.kind_id, catalog).localeCompare(
      outputLabel(b.kind_id, catalog),
    );
  });
  const extraSelected = draftKinds.filter((kind) => !catalog.has(kind));

  useEffect(() => {
    panelRef.current?.focus();
  }, []);

  const toggle = (kind: string, checked: boolean) => {
    setDraftKinds((current) =>
      checked
        ? current.includes(kind)
          ? current
          : [...current, kind]
        : current.filter((k) => k !== kind),
    );
  };
  const changed =
    draftKinds.length !== kinds.length ||
    draftKinds.some((kind, index) => kind !== kinds[index]);

  return (
    <>
      <div
        className="fixed inset-0 z-10"
        onClick={(e) => {
          e.stopPropagation();
          onClose();
        }}
      />
      <div
        ref={panelRef}
        tabIndex={-1}
        role="dialog"
        aria-label="Outputs"
        onClick={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Escape") {
            onClose();
          }
        }}
        className="absolute left-0 top-full z-20 mt-1 w-72 rounded-md border border-zinc-700 bg-zinc-900 shadow-xl outline-none"
      >
        <div className="flex items-baseline justify-between gap-2 border-b border-zinc-800 px-3 py-2">
          <span className="text-sm font-semibold text-zinc-200">Outputs</span>
          <span className="text-xs text-zinc-500">Prepared after accept</span>
        </div>
        <div className="max-h-72 overflow-y-auto py-1">
          {extraSelected.map((kind) => (
            <label
              key={kind}
              className="flex cursor-pointer items-start gap-2 px-3 py-1.5 hover:bg-zinc-800/60"
            >
              <input
                type="checkbox"
                checked
                onChange={(e) => toggle(kind, e.target.checked)}
                className="mt-0.5 h-4 w-4 rounded border-zinc-600 bg-zinc-950 text-sky-600 focus:ring-1 focus:ring-sky-600"
              />
              <span className="min-w-0 font-mono text-xs text-zinc-300">
                {kind}
              </span>
            </label>
          ))}
          {options.map((option) => {
            const checked = draftKinds.includes(option.kind_id);
            return (
              <label
                key={option.kind_id}
                className="flex cursor-pointer items-start gap-2 px-3 py-1.5 hover:bg-zinc-800/60"
              >
                <input
                  type="checkbox"
                  checked={checked}
                  onChange={(e) => toggle(option.kind_id, e.target.checked)}
                  className="mt-0.5 h-4 w-4 rounded border-zinc-600 bg-zinc-950 text-sky-600 focus:ring-1 focus:ring-sky-600"
                />
                <span className="min-w-0">
                  <span className="block text-xs font-medium text-zinc-200">
                    {outputLabel(option.kind_id, catalog)}
                    {!option.produce_available ? (
                      <span className="ml-1.5 font-normal italic text-zinc-500">
                        not available yet
                      </span>
                    ) : null}
                  </span>
                  <span className="block truncate text-xs text-zinc-500">
                    {option.description}
                  </span>
                </span>
              </label>
            );
          })}
        </div>
        <div className="flex items-center justify-end gap-2 border-t border-zinc-800 px-3 py-2">
          <Button variant="ghost" size="sm" onClick={onClose}>
            Cancel
          </Button>
          <Button
            variant="primary"
            size="sm"
            disabled={!changed}
            onClick={() => {
              onSave(draftKinds);
              onClose();
            }}
          >
            Save
          </Button>
        </div>
      </div>
    </>
  );
}

function OutputsControl({
  kinds,
  catalog,
  editable,
  onChange,
}: {
  kinds: string[];
  catalog: Map<string, PacketKindRecord>;
  editable: boolean;
  onChange: (kinds: string[]) => void;
}) {
  const [open, setOpen] = useState(false);

  useEffect(() => {
    if (!editable) setOpen(false);
  }, [editable]);

  const inlineKinds = kinds.slice(0, MAX_INLINE_OUTPUTS);
  const overflowKinds = kinds.slice(MAX_INLINE_OUTPUTS);
  const allLabels = kinds.map((kind) => outputLabel(kind, catalog)).join(", ");

  return (
    <span className="relative inline-flex min-w-0 flex-wrap items-center gap-1.5">
      <span className="shrink-0 text-xs font-medium text-zinc-500">
        Outputs
      </span>
      {kinds.length === 0 ? (
        <span className="text-xs italic text-zinc-500">No outputs</span>
      ) : (
        <span
          className="inline-flex min-w-0 flex-wrap items-center gap-1"
          title={allLabels}
        >
          {inlineKinds.map((kind) => (
            <span
              key={kind}
              title={catalog.get(kind)?.description ?? kind}
              className={`max-w-36 truncate rounded border px-1.5 py-0.5 text-xs sm:max-w-44 ${outputChipCls(kind)}`}
            >
              {outputLabel(kind, catalog)}
            </span>
          ))}
          {overflowKinds.length > 0 ? (
            <span
              title={overflowKinds
                .map((kind) => outputLabel(kind, catalog))
                .join(", ")}
              className="rounded bg-zinc-800/70 px-1.5 py-0.5 text-xs text-zinc-400"
            >
              +{overflowKinds.length}
            </span>
          ) : null}
        </span>
      )}
      {editable ? (
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            setOpen((value) => !value);
          }}
          aria-haspopup="dialog"
          aria-expanded={open}
          className="rounded px-1 py-0.5 text-xs font-medium text-sky-400 hover:bg-zinc-800 hover:text-sky-300 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70"
          title="Choose which outputs are prepared after accept"
        >
          Edit
        </button>
      ) : null}
      {open ? (
        <OutputsPopover
          kinds={kinds}
          catalog={catalog}
          onSave={onChange}
          onClose={() => setOpen(false)}
        />
      ) : null}
    </span>
  );
}

function ProduceGuidanceEditor({
  entry,
  busy,
  onSave,
  onUnauthorized,
}: {
  entry: WorkItemWithRevision;
  busy: boolean;
  onSave: (entry: WorkItemWithRevision, guidance: string) => Promise<void>;
  onUnauthorized: () => void;
}) {
  const [draft, setDraft] = useState(entry.item.produce_guidance);
  const [attachments, setAttachments] = useState<
    { attachment_id: string; filename: string; mime_type: string | null; size_bytes: number | null }[]
  >([]);

  useEffect(() => {
    setDraft(entry.item.produce_guidance);
    let alive = true;
    api
      .workItemSource(entry.item.item_id)
      .then((source) => {
        if (alive) setAttachments(source.message.attachments);
      })
      .catch((err: unknown) => {
        if (isUnauthorized(err)) onUnauthorized();
        else if (alive) setAttachments([]);
      });
    return () => {
      alive = false;
    };
  }, [entry.item.item_id, entry.item.produce_guidance, onUnauthorized]);

  const changed = draft.trim() !== entry.item.produce_guidance.trim();
  const disabled = busy || entry.item.status === "dismissed";
  const addAttachmentContext = (attachment: { attachment_id: string; filename: string; mime_type: string | null; size_bytes: number | null }) => {
    const line = `Use attachment ${attachment.filename} (${attachment.mime_type ?? "unknown"}, ${formatBytes(
      attachment.size_bytes,
    )}; attachment_id=${attachment.attachment_id}) as context if this draft/task needs it.`;
    if (draft.includes(attachment.attachment_id)) return;
    setDraft((current) => [current.trim(), line].filter(Boolean).join("\n"));
  };

  return (
    <div className="border-t border-zinc-800/90 bg-zinc-950/50 px-4 py-3">
      <textarea
        value={draft}
        onChange={(event) => setDraft(event.target.value)}
        disabled={disabled}
        maxLength={2000}
        rows={3}
        className="min-h-20 w-full resize-y rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 outline-none placeholder:text-zinc-500 focus-visible:border-sky-500 focus-visible:ring-2 focus-visible:ring-sky-500/70 disabled:opacity-60"
        placeholder="Context or guidance for AI-generated drafts"
      />
      {attachments.length > 0 ? (
        <div className="mt-2 flex flex-wrap gap-2">
          {attachments.map((attachment) => (
            <Button
              key={attachment.attachment_id}
              variant="secondary"
              size="sm"
              disabled={disabled || draft.includes(attachment.attachment_id)}
              onClick={() => addAttachmentContext(attachment)}
              title={`Add ${attachment.filename} to draft context`}
            >
              Add {attachment.filename}
            </Button>
          ))}
        </div>
      ) : null}
      <div className="mt-2 flex items-center justify-between gap-2">
        <span className="text-xs tabular-nums text-zinc-500">
          {draft.trim().length}/2000
        </span>
        <div className="flex items-center gap-2">
          {entry.item.produce_guidance ? (
            <Button
              variant="secondary"
              size="sm"
              disabled={disabled}
              onClick={() => void onSave(entry, "")}
            >
              Clear
            </Button>
          ) : null}
          <Button
            variant="success"
            size="sm"
            disabled={disabled || !changed}
            busy={busy}
            onClick={() => void onSave(entry, draft)}
          >
            Save
          </Button>
        </div>
      </div>
    </div>
  );
}

// Operator power tool (gated by BOS_AGENT_LAUNCH_ENABLED): open a Agent Monitor
// agent session seeded with this item's email/details, plus optional notes.
function LaunchAgentPanel({
  itemId,
  defaultContext,
  defaultWorkDir,
  onUnauthorized,
}: {
  itemId: string;
  defaultContext: string;
  defaultWorkDir: string;
  onUnauthorized: () => void;
}) {
  const [context, setContext] = useState(defaultContext);
  const [workDir, setWorkDir] = useState(defaultWorkDir);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<LaunchAgentResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [attachments, setAttachments] = useState<
    { attachment_id: string; filename: string; mime_type: string | null; size_bytes: number | null }[]
  >([]);
  const [selectedAttachmentIds, setSelectedAttachmentIds] = useState<string[]>([]);

  useEffect(() => {
    setContext(defaultContext);
    setWorkDir(defaultWorkDir);
    setResult(null);
    setError(null);
    setSelectedAttachmentIds([]);
    let alive = true;
    api
      .workItemSource(itemId)
      .then((source) => {
        if (alive) setAttachments(source.message.attachments);
      })
      .catch((err: unknown) => {
        if (isUnauthorized(err)) onUnauthorized();
        else if (alive) setAttachments([]);
      });
    return () => {
      alive = false;
    };
  }, [defaultContext, defaultWorkDir, itemId]);

  const launch = async () => {
    setBusy(true);
    setError(null);
    try {
      const res = await api.launchAgent(itemId, {
        context,
        work_dir: workDir.trim().length > 0 ? workDir.trim() : null,
        attachment_ids: selectedAttachmentIds,
        idempotency_key: crypto.randomUUID(),
      });
      setResult(res);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="border-t border-zinc-800/90 bg-zinc-950/50 px-4 py-3">
      <label className="mb-2 flex flex-col gap-1 text-xs text-zinc-400">
        Agent workdir
        <input
          value={workDir}
          onChange={(event) => setWorkDir(event.target.value)}
          disabled={busy}
          className="w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2 font-mono text-sm text-zinc-100 outline-none placeholder:text-zinc-500 focus-visible:border-sky-500 focus-visible:ring-2 focus-visible:ring-sky-500/70 disabled:opacity-60"
          placeholder="/home/example/projects/BusinessOS"
        />
      </label>
      <textarea
        value={context}
        onChange={(event) => setContext(event.target.value)}
        disabled={busy}
        maxLength={2000}
        rows={3}
        className="min-h-20 w-full resize-y rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 outline-none placeholder:text-zinc-500 focus-visible:border-sky-500 focus-visible:ring-2 focus-visible:ring-sky-500/70 disabled:opacity-60"
        placeholder="Optional notes for the agent — the item's email and details are included automatically."
      />
      {attachments.length > 0 ? (
        <div className="mt-2 rounded-md border border-zinc-800 bg-zinc-950/60 p-2">
          <div className="mb-1 text-xs font-semibold text-zinc-300">
            Stage attachments into workdir
          </div>
          <div className="space-y-1">
            {attachments.map((attachment) => {
              const checked = selectedAttachmentIds.includes(attachment.attachment_id);
              return (
                <label
                  key={attachment.attachment_id}
                  className="flex min-w-0 items-center gap-2 text-xs text-zinc-300"
                >
                  <input
                    type="checkbox"
                    checked={checked}
                    disabled={busy}
                    onChange={(event) => {
                      setSelectedAttachmentIds((current) =>
                        event.target.checked
                          ? [...current, attachment.attachment_id]
                          : current.filter((id) => id !== attachment.attachment_id),
                      );
                    }}
                  />
                  <span className="min-w-0 flex-1 truncate">
                    {attachment.filename}
                    <span className="text-zinc-500">
                      {" "}
                      {attachment.mime_type ?? "unknown"} ·{" "}
                      {formatBytes(attachment.size_bytes)}
                    </span>
                  </span>
                </label>
              );
            })}
          </div>
        </div>
      ) : null}
      <div className="mt-2 flex items-center justify-between gap-2">
        <span className="text-xs tabular-nums text-zinc-500">
          {context.trim().length}/2000
        </span>
        <Button
          variant="success"
          size="sm"
          busy={busy}
          disabled={busy}
          onClick={() => void launch()}
        >
          Launch agent
        </Button>
      </div>
      {error ? <p className="mt-2 text-xs text-rose-400">{error}</p> : null}
      {result ? (
        <p className="mt-2 text-xs text-zinc-400">
          Agent session started —{" "}
          <span className="font-mono text-zinc-300">{result.session_id}</span>
          {result.thread_id ? ` (thread ${result.thread_id})` : ""}.
          {result.staged_evidence_paths.length > 0
            ? ` Staged ${result.staged_evidence_paths.length} attachment file${
                result.staged_evidence_paths.length === 1 ? "" : "s"
              }.`
            : ""}
        </p>
      ) : null}
    </div>
  );
}

export default function Queue({
  onUnauthorized,
  helpTopicId,
  onOpenHelpTopic,
  onOpenTasks,
  debugEnabled,
  agentLaunchEnabled,
  focusItemId,
  onFocusItemConsumed,
  onOpenDebug,
  onCreateOutput,
}: {
  onUnauthorized: () => void;
  helpTopicId?: string;
  onOpenHelpTopic: (topicId: string) => void;
  onOpenTasks: () => void;
  debugEnabled: boolean;
  agentLaunchEnabled: boolean;
  focusItemId?: string | null;
  onFocusItemConsumed?: () => void;
  onOpenDebug: (diagnosticId?: string) => void;
  onCreateOutput?: () => void;
}) {
  // Fetch all statuses in one call for the normal lanes. The source-attention
  // lane uses a separate server-side filter so older high-attention items do
  // not disappear behind the generic route cap.
  const [items, setItems] = useState<WorkItemWithRevision[]>([]);
  const [attentionItems, setAttentionItems] = useState<WorkItemWithRevision[]>([]);
  const [filter, setFilter] = useState<Filter>("needs_you");
  const [sortMode, setSortMode] = useState<SortMode>("status");
  const [loaded, setLoaded] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<Notice>(null);
  const [busyItemId, setBusyItemId] = useState<string | null>(null);
  const [expandedItemId, setExpandedItemId] = useState<string | null>(null);
  const [sourceItemId, setSourceItemId] = useState<string | null>(null);
  const [guidanceItemId, setGuidanceItemId] = useState<string | null>(null);
  const [launchItemId, setLaunchItemId] = useState<string | null>(null);
  // Set after Accept so the item stays in view (whatever the lane says) with
  // its drafts panel open — the decision continues instead of stranding.
  const [followedItemId, setFollowedItemId] = useState<string | null>(null);
  const [noteOpen, setNoteOpen] = useState(false);
  const [noteBody, setNoteBody] = useState("");
  const [noteSaving, setNoteSaving] = useState(false);
  // D2: the +Log note form's action checkboxes. Selecting one accepts the item
  // and produces that kind immediately. CRM note + records are pre-checked.
  const [noteActions, setNoteActions] = useState<string[]>([
    "crm_activity",
    "crm_record_create",
  ]);
  // Keyboard focus row (j/k loop) — an index into the visible list.
  const [focusIdx, setFocusIdx] = useState(0);
  const rowRefs = useRef(new Map<string, HTMLElement>());
  // Source-account context: who am I, and user_id → display name for badges.
  const [meId, setMeId] = useState<string | null>(null);
  const [userNames, setUserNames] = useState<Map<string, string>>(new Map());
  const [mineOnly, setMineOnly] = useState(false);
  // Watchdog needs-you surface: overdue follow-up tasks (count + worst).
  const [overdueTasks, setOverdueTasks] = useState<TaskEscalation[]>([]);
  const packetKindCatalog = usePacketKinds();
  const { categories } = useCategories();
  const categoriesById = new Map(
    categories.map((category) => [category.category_id, category]),
  );
  const packetKindsById = new Map(
    packetKindCatalog.map((k) => [k.kind_id, k]),
  );

  const load = useCallback(async () => {
    setRefreshing(true);
    try {
      const [res, attentionRes] = await Promise.all([
        api.workQueue(),
        api.workQueue({ attentionLevel: "higher" }),
      ]);
      setItems(res.items);
      setAttentionItems(attentionRes.items);
      setError(null);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    } finally {
      setRefreshing(false);
      setLoaded(true);
    }
    // Overdue-task strip degrades quietly; the queue itself never blocks on it.
    try {
      const res = await api.tasks("open", localToday());
      setOverdueTasks(
        res.tasks
          .map((t) => t.escalation)
          .filter((e): e is TaskEscalation => e?.lane === "overdue"),
      );
    } catch {
      setOverdueTasks([]);
    }
  }, [onUnauthorized]);

  // Identity + user names load once; failures degrade to raw ids quietly.
  useEffect(() => {
    void (async () => {
      try {
        const [me, users] = await Promise.all([api.whoami(), api.users()]);
        setMeId(me.actor_id);
        setUserNames(
          new Map(users.users.map((u) => [u.user_id, u.display_name])),
        );
      } catch {
        // Badges fall back to raw ids; the Mine filter stays hidden.
      }
    })();
  }, []);

  // Command palette integrations.
  useAppCommand("refresh", () => void load());
  useAppCommand("queue.log-note", () => setNoteOpen(true));

  // Poll faster while drafts are being produced so finished drafts re-enter
  // the lane promptly (same pattern as the panels' delivery polling).
  const producePending = items.some(
    (e) =>
      e.item.status === "accepted" && e.pending_produce_kinds.length > 0,
  );
  usePolling(load, {
    intervalMs: producePending ? FAST_POLL_INTERVAL_MS : POLL_INTERVAL_MS,
  });

  const runAction = async (
    entry: WorkItemWithRevision,
    action: WorkItemActionKind,
  ) => {
    if (
      action === "trash" &&
      !window.confirm(
        `Move the source email for “${entry.item.title}” to Gmail Trash? This also dismisses the Queue item.`,
      )
    ) {
      return;
    }
    setBusyItemId(entry.item.item_id);
    setNotice(null);

    // Snapshot for revert on error.
    const snapshot = entry;
    const draftKindsToProduce =
      action === "accept" && entry.item.status === "open"
        ? draftKindsFor(entry).filter(
            (kind) =>
              !entry.staged_draft_kinds.includes(kind) &&
              !entry.pending_produce_kinds.includes(kind),
          )
        : [];

    // Determine optimistic target status.
    const optimisticStatus =
      action === "accept"
        ? "accepted"
        : action === "dismiss" || action === "trash"
          ? "dismissed"
          : "open";

    // Apply optimistic patch immediately.
    setItems((prev) =>
      prev.map((e) =>
        e.item.item_id === entry.item.item_id
          ? {
              ...e,
              item: { ...e.item, status: optimisticStatus },
              pending_produce_kinds:
                draftKindsToProduce.length > 0
                  ? [
                      ...new Set([
                        ...e.pending_produce_kinds,
                        ...draftKindsToProduce,
                      ]),
                    ]
                  : e.pending_produce_kinds,
            }
          : e,
      ),
    );

    // Accept side-effects fire immediately, before request resolves.
    if (action === "accept") {
      setFollowedItemId(entry.item.item_id);
      setExpandedItemId(entry.item.item_id);
    } else if (followedItemId === entry.item.item_id) {
      setFollowedItemId(null);
    }

    try {
      const res = await api.workItemAction(entry.item.item_id, {
        action,
        expected_revision: entry.revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      // Patch revision from server response.
      const newRevision = res.revision ?? snapshot.revision + 1;
      setItems((prev) =>
        prev.map((e) =>
          e.item.item_id === entry.item.item_id
            ? { ...e, revision: newRevision }
            : e,
        ),
      );
      if (draftKindsToProduce.length > 0) {
        const results = await Promise.allSettled(
          draftKindsToProduce.map((kind) =>
            produceDraftForKind(entry.item.item_id, kind),
          ),
        );
        const rejected = results.find((result) => result.status === "rejected");
        if (rejected && rejected.status === "rejected") {
          setNotice({
            text: `Accepted, but draft production failed: ${errorMessage(rejected.reason)}`,
            kind: "error",
          });
        }
      }
      // Silent background reconcile — server-computed fields catch up without blocking.
      void load();
    } catch (err) {
      if (isUnauthorized(err)) {
        onUnauthorized();
      } else if (isRevisionConflict(err)) {
        setNotice({
          text: "Item changed elsewhere — reloaded the latest queue.",
          kind: "conflict",
        });
        await load();
      } else {
        // Revert: restore snapshot entry by id.
        setItems((prev) =>
          prev.map((e) =>
            e.item.item_id === snapshot.item.item_id ? snapshot : e,
          ),
        );
        // Also revert follow/expand side-effects if this was an accept.
        if (action === "accept") {
          setFollowedItemId(null);
          setExpandedItemId(null);
        }
        setNotice({ text: `Action failed: ${errorMessage(err)}`, kind: "error" });
      }
    } finally {
      setBusyItemId(null);
    }
  };

  const produceMissingDrafts = async (entry: WorkItemWithRevision) => {
    const kindsToProduce = draftKindsFor(entry).filter(
      (kind) =>
        !entry.staged_draft_kinds.includes(kind) &&
        !entry.pending_produce_kinds.includes(kind),
    );
    setExpandedItemId(entry.item.item_id);
    setFollowedItemId(entry.item.item_id);
    if (kindsToProduce.length === 0) return;

    setBusyItemId(entry.item.item_id);
    setNotice(null);
    setItems((prev) =>
      prev.map((e) =>
        e.item.item_id === entry.item.item_id
          ? {
              ...e,
              pending_produce_kinds: [
                ...new Set([
                  ...e.pending_produce_kinds,
                  ...kindsToProduce,
                ]),
              ],
            }
          : e,
      ),
    );

    try {
      const results = await Promise.allSettled(
        kindsToProduce.map((kind) =>
          produceDraftForKind(entry.item.item_id, kind),
        ),
      );
      const failedKinds = kindsToProduce.filter(
        (_, index) => results[index]?.status === "rejected",
      );
      const firstRejected = results.find(
        (result) => result.status === "rejected",
      );
      if (firstRejected && firstRejected.status === "rejected") {
        if (isUnauthorized(firstRejected.reason)) onUnauthorized();
        setItems((prev) =>
          prev.map((e) =>
            e.item.item_id === entry.item.item_id
              ? {
                  ...e,
                  pending_produce_kinds: e.pending_produce_kinds.filter(
                    (kind) => !failedKinds.includes(kind),
                  ),
                }
              : e,
          ),
        );
        setNotice({
          text: `Draft production failed: ${errorMessage(firstRejected.reason)}`,
          kind: "error",
        });
      }
      await load();
    } finally {
      setBusyItemId(null);
    }
  };

  const updateKinds = async (entry: WorkItemWithRevision, kinds: string[]) => {
    setBusyItemId(entry.item.item_id);
    setNotice(null);

    // Snapshot for revert on error.
    const snapshot = entry;

    // Apply optimistic patch immediately.
    setItems((prev) =>
      prev.map((e) =>
        e.item.item_id === entry.item.item_id
          ? { ...e, item: { ...e.item, packet_kinds: kinds } }
          : e,
      ),
    );

    try {
      const res = await api.workItemPacketKinds(entry.item.item_id, {
        packet_kinds: kinds,
        expected_revision: entry.revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      // Patch revision from server response.
      const newRevision = res.revision ?? snapshot.revision + 1;
      setItems((prev) =>
        prev.map((e) =>
          e.item.item_id === entry.item.item_id
            ? { ...e, revision: newRevision }
            : e,
        ),
      );
      // Silent background reconcile.
      void load();
    } catch (err) {
      if (isUnauthorized(err)) {
        onUnauthorized();
      } else if (isRevisionConflict(err)) {
        setNotice({
          text: "Item changed elsewhere — reloaded the latest queue.",
          kind: "conflict",
        });
        await load();
      } else {
        // Revert: restore snapshot entry by id.
        setItems((prev) =>
          prev.map((e) =>
            e.item.item_id === snapshot.item.item_id ? snapshot : e,
          ),
        );
        setNotice({ text: `Kind change failed: ${errorMessage(err)}`, kind: "error" });
      }
    } finally {
      setBusyItemId(null);
    }
  };

  const updateGuidance = async (
    entry: WorkItemWithRevision,
    produceGuidance: string,
  ) => {
    setBusyItemId(entry.item.item_id);
    setNotice(null);
    const snapshot = entry;
    const nextGuidance = produceGuidance.trim();

    setItems((prev) =>
      prev.map((e) =>
        e.item.item_id === entry.item.item_id
          ? { ...e, item: { ...e.item, produce_guidance: nextGuidance } }
          : e,
      ),
    );

    try {
      const res = await api.workItemProduceGuidance(entry.item.item_id, {
        produce_guidance: nextGuidance,
        expected_revision: entry.revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      const newRevision = res.revision ?? snapshot.revision + 1;
      setItems((prev) =>
        prev.map((e) =>
          e.item.item_id === entry.item.item_id
            ? { ...e, revision: newRevision }
            : e,
        ),
      );
      void load();
    } catch (err) {
      if (isUnauthorized(err)) {
        onUnauthorized();
      } else if (isRevisionConflict(err)) {
        setNotice({
          text: "Item changed elsewhere — reloaded the latest queue.",
          kind: "conflict",
        });
        await load();
      } else {
        setItems((prev) =>
          prev.map((e) =>
            e.item.item_id === snapshot.item.item_id ? snapshot : e,
          ),
        );
        setNotice({ text: `Context save failed: ${errorMessage(err)}`, kind: "error" });
      }
    } finally {
      setBusyItemId(null);
    }
  };

  const updateAssignment = async (
    entry: WorkItemWithRevision,
    action: "assign_to_me" | "unassign",
  ) => {
    setBusyItemId(entry.item.item_id);
    setNotice(null);
    const snapshot = entry;
    const nextAssignee =
      action === "assign_to_me" ? meId : null;

    setItems((prev) =>
      prev.map((e) =>
        e.item.item_id === entry.item.item_id
          ? { ...e, item: { ...e.item, assignee_user_id: nextAssignee } }
          : e,
      ),
    );

    try {
      const res = await api.workItemAssignment(entry.item.item_id, {
        action,
        assignee_user_id: null,
        expected_revision: entry.revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      const newRevision = res.revision ?? snapshot.revision + 1;
      setItems((prev) =>
        prev.map((e) =>
          e.item.item_id === entry.item.item_id
            ? { ...e, revision: newRevision }
            : e,
        ),
      );
      void load();
    } catch (err) {
      if (isUnauthorized(err)) {
        onUnauthorized();
      } else if (isRevisionConflict(err)) {
        setNotice({
          text: "Item changed elsewhere — reloaded the latest queue.",
          kind: "conflict",
        });
        await load();
      } else {
        setItems((prev) =>
          prev.map((e) =>
            e.item.item_id === snapshot.item.item_id ? snapshot : e,
          ),
        );
        setNotice({ text: `Assignment failed: ${errorMessage(err)}`, kind: "error" });
      }
    } finally {
      setBusyItemId(null);
    }
  };

  // "Mine" = assigned to me first. Personal, non-shared items keep their
  // previous source-owned meaning until explicitly assigned.
  const mineAvailable =
    meId !== null && meId !== "operator" &&
    [...items, ...attentionItems].some((e) => assignedOrPersonalMine(e, meId));
  const base =
    mineOnly && mineAvailable
      ? items.filter((e) => assignedOrPersonalMine(e, meId))
      : items;
  const attentionBase =
    mineOnly && mineAvailable
      ? attentionItems.filter((e) => assignedOrPersonalMine(e, meId))
      : attentionItems;
  const visibleBase = filter === "attention" ? attentionBase : base;
  const counts = new Map<Filter, number>(
    FILTERS.map((f) => [
      f.id,
      f.id === "attention"
        ? attentionBase.length
        : base.filter((e) => matchesFilter(e, f.id)).length,
    ]),
  );
  const visible = visibleBase
    .filter((e) => matchesFilter(e, filter) || e.item.item_id === followedItemId)
    .sort((a, b) => {
      if (sortMode === "status") {
        const rank = statusRank(a) - statusRank(b);
        if (rank !== 0) return rank;
        const attention = attentionRank(a) - attentionRank(b);
        if (attention !== 0) return attention;
      }
      return b.item.created_at_ms - a.item.created_at_ms;
    });
  const focusedEntry =
    visible.length > 0
      ? visible[Math.min(focusIdx, visible.length - 1)]
      : undefined;
  const focusedItemId = focusedEntry?.item.item_id ?? null;

  useEffect(() => {
    if (focusedItemId) {
      rowRefs.current.get(focusedItemId)?.scrollIntoView({ block: "nearest" });
    }
  }, [focusedItemId]);

  const focusedStatus = focusedEntry?.item.status;
  const focusedHasDraftWorkspace =
    focusedEntry != null && draftKindsFor(focusedEntry).length > 0;
  useEffect(() => {
    if (!focusedItemId) return;
    if (focusedStatus === "accepted" && focusedHasDraftWorkspace) {
      setExpandedItemId(focusedItemId);
      setSourceItemId(null);
    } else {
      setSourceItemId(focusedItemId);
      setExpandedItemId(null);
    }
  }, [focusedHasDraftWorkspace, focusedItemId, focusedStatus]);

  useEffect(() => {
    if (!focusItemId || !loaded) return;
    const idx = visible.findIndex((entry) => entry.item.item_id === focusItemId);
    if (idx === -1) {
      onFocusItemConsumed?.();
      return;
    }
    const entry = visible[idx];
    setFocusIdx(idx);
    setFollowedItemId(focusItemId);
    if (entry.item.status === "accepted" && draftKindsFor(entry).length > 0) {
      setExpandedItemId(focusItemId);
      setSourceItemId(null);
    } else {
      setSourceItemId(focusItemId);
      setExpandedItemId(null);
    }
    requestAnimationFrame(() => {
      rowRefs.current.get(focusItemId)?.scrollIntoView({ block: "center" });
    });
    onFocusItemConsumed?.();
  }, [focusItemId, loaded, onFocusItemConsumed, visible]);

  // Keyboard loop (Superhuman/Linear momentum): j/k move, enter expands the
  // focused row's detail (drafts when accepted+producible, else source),
  // a accepts and produces, x dismisses. Inputs and modifier chords pass through.
  // No dep array: re-subscribe each render so the handler sees fresh state.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const target = e.target as HTMLElement | null;
      if (
        document.querySelector('[role="dialog"]') !== null ||
        target instanceof HTMLInputElement ||
        target instanceof HTMLTextAreaElement ||
        target instanceof HTMLSelectElement ||
        (target?.isContentEditable ?? false)
      ) {
        return;
      }
      switch (e.key) {
        case "j":
          e.preventDefault();
          setFocusIdx((i) => Math.min(i + 1, Math.max(visible.length - 1, 0)));
          break;
        case "k":
          e.preventDefault();
          setFocusIdx((i) => Math.max(Math.min(i, visible.length - 1) - 1, 0));
          break;
        case "Enter": {
          if (!focusedEntry) return;
          e.preventDefault();
          const id = focusedEntry.item.item_id;
          const hasDraftsPanel =
            focusedEntry.item.status === "accepted" &&
            draftKindsFor(focusedEntry).length > 0;
          if (hasDraftsPanel) {
            const missingDraftKinds = draftKindsFor(focusedEntry).filter(
              (kind) =>
                !focusedEntry.staged_draft_kinds.includes(kind) &&
                !focusedEntry.pending_produce_kinds.includes(kind),
            );
            if (missingDraftKinds.length > 0 && busyItemId === null) {
              void produceMissingDrafts(focusedEntry);
            } else {
              setExpandedItemId((prev) => (prev === id ? null : id));
            }
          } else {
            setSourceItemId((prev) => (prev === id ? null : id));
          }
          break;
        }
        case "Escape":
          e.preventDefault();
          if (expandedItemId !== null) {
            setExpandedItemId(null);
          } else if (sourceItemId !== null) {
            setSourceItemId(null);
          }
          break;
        case "a":
          if (
            focusedEntry &&
            focusedEntry.item.status === "open" &&
            draftKindsFor(focusedEntry).length > 0 &&
            busyItemId === null
          ) {
            e.preventDefault();
            void runAction(focusedEntry, "accept");
          }
          break;
        case "x":
          if (
            focusedEntry &&
            focusedEntry.item.status !== "dismissed" &&
            busyItemId === null
          ) {
            e.preventDefault();
            void runAction(focusedEntry, "dismiss");
          }
          break;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <h2 className="text-lg font-semibold text-zinc-100">Work queue</h2>
          <SectionHelpButton
            topicId={helpTopicId}
            onOpenHelp={onOpenHelpTopic}
            label="Open help for Queue"
          />
        </div>
        <div className="flex items-center gap-3">
          <span
            className="hidden text-xs text-zinc-500 sm:inline"
            title="Keyboard: j/k move focus, Enter toggles details (drafts or source), Esc collapse, a accepts and produces a draft, x dismisses"
          >
            j/k · Enter details · a accept · x dismiss
          </span>
          <span className="text-xs text-zinc-400">polls every 30s</span>
          {onCreateOutput ? (
            <Button
              variant="primary"
              size="sm"
              onClick={onCreateOutput}
              title="Create a typed output without an inbound source"
            >
              + Create output
            </Button>
          ) : null}
          <Button
            variant="primary"
            size="sm"
            onClick={() => setNoteOpen((open) => !open)}
            title="Log a call you took, a walk-in, or a reminder — it becomes a work item immediately"
          >
            + Log note
          </Button>
          <Button
            variant="secondary"
            size="sm"
            onClick={() => void load()}
            busy={refreshing}
          >
            {refreshing ? "Refreshing…" : "Refresh"}
          </Button>
        </div>
      </div>

      {overdueTasks.length > 0 ? (
        <button
          onClick={onOpenTasks}
          className={`flex items-center gap-2 rounded-lg border px-3 py-2 text-left text-sm transition hover:brightness-110 ${
            overdueTasks.some((e) => e.level === "critical")
              ? "border-red-700 bg-red-950/60 text-red-200"
              : "border-red-900/70 bg-red-950/30 text-red-300"
          }`}
          title="Open the Tasks tab"
        >
          <span className="font-semibold">
            {overdueTasks.length} overdue follow-up task
            {overdueTasks.length === 1 ? "" : "s"}
          </span>
          <span className="text-xs opacity-80">
            worst {Math.max(...overdueTasks.map((e) => e.days_overdue))}d overdue
            {overdueTasks.some((e) => e.level === "critical")
              ? " · critical"
              : ""}{" "}
            — review in Tasks →
          </span>
        </button>
      ) : null}

      {noteOpen ? (
        <div className="rounded-lg border border-zinc-800 bg-zinc-900/60 p-3">
          <textarea
            value={noteBody}
            onChange={(e) => setNoteBody(e.target.value)}
            placeholder="Dana called — wants the storefront quote by Friday…  (first line becomes the item title)"
            rows={3}
            autoFocus
            className="w-full rounded-md border border-zinc-700 bg-zinc-950 px-3 py-2 text-sm text-zinc-200 placeholder:text-zinc-500 focus:border-sky-600 focus:outline-none"
          />
          <div className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-2">
            <span className="text-xs text-zinc-500">Spin up:</span>
            {NOTE_ACTIONS.map((action) => {
              const checked = noteActions.includes(action.kind);
              return (
                <label
                  key={action.kind}
                  className="flex cursor-pointer items-center gap-1.5 text-sm text-zinc-300"
                >
                  <input
                    type="checkbox"
                    checked={checked}
                    onChange={(e) =>
                      setNoteActions((prev) =>
                        e.target.checked
                          ? [...prev, action.kind]
                          : prev.filter((k) => k !== action.kind),
                      )
                    }
                    className="h-4 w-4 rounded border-zinc-600 bg-zinc-950 text-sky-600 focus:ring-1 focus:ring-sky-600"
                  />
                  {action.label}
                </label>
              );
            })}
          </div>
          <div className="mt-2 flex items-center justify-end gap-2">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                setNoteOpen(false);
                setNoteBody("");
                setNoteActions(["crm_activity", "crm_record_create"]);
              }}
            >
              Cancel
            </Button>
            <Button
              variant="primary"
              size="sm"
              busy={noteSaving}
              disabled={noteBody.trim().length === 0}
              onClick={() => {
                setNoteSaving(true);
                setNotice(null);
                void (async () => {
                  try {
                    await api.createOperatorNote({
                      body: noteBody.trim(),
                      idempotency_key: crypto.randomUUID(),
                      actor_id: null,
                      actions: noteActions,
                    });
                    setNoteOpen(false);
                    setNoteBody("");
                    setNoteActions(["crm_activity", "crm_record_create"]);
                    await load();
                  } catch (err) {
                    if (isUnauthorized(err)) onUnauthorized();
                    else setNotice({ text: `Note failed: ${errorMessage(err)}`, kind: "error" });
                  } finally {
                    setNoteSaving(false);
                  }
                })();
              }}
            >
              {noteSaving ? "Saving…" : "Save → queue"}
            </Button>
          </div>
        </div>
      ) : null}

      <div className="surface-section-head surface-head-violet flex flex-wrap items-center gap-2">
        {FILTERS.map((f) => {
          const active = filter === f.id;
          const count = counts.get(f.id) ?? 0;
          return (
            <button
              key={f.id}
              onClick={() => {
                setFilter(f.id);
                setFollowedItemId(null);
                setFocusIdx(0);
              }}
              className={`rounded-full px-3 py-1 text-xs font-medium transition ${
                active
                  ? FILTER_ACTIVE_CLS[f.id]
                  : "text-zinc-400 hover:bg-zinc-900 hover:text-zinc-200"
              }`}
            >
              {f.label}
              <span
                className={`ml-1.5 tabular-nums ${
                  active
                    ? "opacity-70"
                    : f.id === "attention" && count > 0
                      ? "font-semibold text-amber-400"
                      : "text-zinc-500"
                }`}
              >
                {count}
              </span>
            </button>
          );
        })}
        {mineAvailable ? (
          <button
            onClick={() => {
              setMineOnly((v) => !v);
              setFocusIdx(0);
            }}
            className={`ml-2 rounded-full px-3 py-1 text-xs font-medium transition ${
              mineOnly
                ? "bg-teal-900/60 text-teal-200 ring-1 ring-inset ring-teal-600"
                : "text-zinc-400 hover:bg-zinc-900 hover:text-zinc-200"
            }`}
            title="Only items assigned to you, plus your personal non-shared items"
          >
            Mine
          </button>
        ) : null}
        <div className="ml-auto flex items-center gap-1 rounded-full border border-zinc-800 bg-zinc-950/60 p-0.5">
          {([
            ["newest", "Newest"],
            ["status", "Status"],
          ] as const).map(([id, label]) => (
            <button
              key={id}
              onClick={() => setSortMode(id)}
              className={`rounded-full px-2.5 py-0.5 text-xs font-medium transition ${
                sortMode === id
                  ? "bg-zinc-800 text-zinc-100"
                  : "text-zinc-500 hover:text-zinc-200"
              }`}
              title={
                id === "status"
                  ? "Sort by item state first: failed, ready for review, drafting, open, accepted, dismissed"
                  : "Sort newest first"
              }
            >
              {label}
            </button>
          ))}
        </div>
      </div>

      {error ? (
        <div className="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300">
          Failed to load work queue: {error}
        </div>
      ) : null}
      {notice ? (
        <div
          className={`rounded-md border px-3 py-2 text-sm ${
            notice.kind === "conflict"
              ? "border-amber-900/60 bg-amber-950/30 text-amber-300"
              : "border-red-900/60 bg-red-950/40 text-red-300"
          }`}
        >
          {notice.text}
        </div>
      ) : null}

      {loaded && visible.length === 0 && !error ? (
        filter === "needs_you" ? (
          <EmptyState variant="celebrate" title="Queue clear — nothing needs you.">
            New items and staged drafts land here. Items appear when a
            category&apos;s policy has &ldquo;creates work items&rdquo;
            enabled — enable it per category in Categories. Newly enabled
            categories will also include existing email (added within about 2 minutes).
          </EmptyState>
        ) : (
          <EmptyState title={`No ${filter === "all" ? "" : `${filter} `}items${filter === "all" ? " in the queue" : ""}.`} />
        )
      ) : null}

      {visible.length > 0 ? (
        <div className="grid min-h-[38rem] gap-3 lg:grid-cols-[19rem_minmax(0,1fr)]">
          <nav
            aria-label="Work item navigator"
            className="max-h-56 overflow-y-auto rounded-lg border border-zinc-800 bg-zinc-950/60 p-1 lg:max-h-[calc(100vh-14rem)]"
          >
            {visible.map((entry, idx) => {
              const selected = entry.item.item_id === focusedItemId;
              return (
                <button
                  key={entry.item.item_id}
                  ref={(element) => {
                    if (element) rowRefs.current.set(entry.item.item_id, element);
                    else rowRefs.current.delete(entry.item.item_id);
                  }}
                  onClick={() => setFocusIdx(idx)}
                  className={`relative mb-1 w-full overflow-hidden rounded-md border px-3 py-2 text-left transition last:mb-0 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70 ${
                    selected
                      ? "border-sky-500/60 bg-sky-500/10"
                      : "border-transparent hover:border-zinc-800 hover:bg-zinc-900"
                  }`}
                >
                  <span
                    aria-hidden
                    className={`absolute inset-y-0 left-0 w-1 ${rowRailCls(entry)}`}
                  />
                  <span className="block truncate text-sm font-medium text-zinc-100">
                    {entry.item.title || "(untitled)"}
                  </span>
                  <span className="mt-1 flex items-center justify-between gap-2 text-xs text-zinc-400">
                    <span className="truncate">
                      {entry.failure_notifications.length > 0
                        ? "Task failed"
                        : entry.staged_draft_kinds.length > 0
                          ? "Draft ready"
                          : entry.pending_produce_kinds.length > 0
                            ? "Drafting…"
                            : entry.item.status}
                    </span>
                    <span className="shrink-0 tabular-nums">
                      {relativeAge(entry.item.created_at_ms)}
                    </span>
                  </span>
                </button>
              );
            })}
          </nav>
          <div className="min-w-0">
          {[focusedEntry].map((entry) => {
            if (!entry) return null;
            const idx = visible.findIndex(
              (candidate) => candidate.item.item_id === entry.item.item_id,
            );
            const item = entry.item;
            const busy = busyItemId === item.item_id;
            const stagedKinds =
              item.status === "accepted" ? entry.staged_draft_kinds : [];
            const pendingKinds =
              item.status === "accepted" ? entry.pending_produce_kinds : [];
            const failureNotifications =
              item.status === "accepted" ? entry.failure_notifications : [];
            const firstDebugNotification = failureNotifications.find(
              (notification) => notification.diagnostic_id,
            );
            const draftKinds = draftKindsFor(entry);
            const hasProduceKind = draftKinds.length > 0;
            const missingDraftKinds = draftKinds.filter(
              (kind) =>
                !stagedKinds.includes(kind) && !pendingKinds.includes(kind),
            );
            const hasStagedDraft = stagedKinds.length > 0;
            const expanded = expandedItemId === item.item_id;
            const sourceOpen = sourceItemId === item.item_id;
            const isFocused = focusedItemId === item.item_id;
            const itemSharedVisible = sharedVisible(entry);
            const canAssignToMe =
              itemSharedVisible &&
              meId !== null &&
              meId !== "operator" &&
              item.assignee_user_id !== meId;
            const canUnassign =
              item.assignee_user_id !== null &&
              (item.assignee_user_id === meId || meId === "operator");
            return (
              <div
                key={item.item_id}
                className={`relative rounded-lg border transition ${
                  isFocused
                    ? "border-sky-500/60 bg-zinc-900/60 ring-1 ring-inset ring-sky-500/30"
                    : "border-zinc-800 bg-zinc-900/40 hover:border-zinc-700"
                } ${item.status === "dismissed" ? "opacity-75" : ""}`}
              >
                <span
                  aria-hidden
                  className={`absolute inset-y-0 left-0 w-1 rounded-l-lg ${rowRailCls(entry)}`}
                />
                <div
                  onClick={() => setFocusIdx(idx)}
                  className={`relative flex flex-wrap items-start gap-x-3 gap-y-2 py-2.5 pl-4 pr-3 hover:bg-zinc-900/60 ${
                    sourceOpen ||
                    guidanceItemId === item.item_id ||
                    launchItemId === item.item_id ||
                    expanded
                      ? "rounded-t-lg border-b border-zinc-800/80"
                      : "rounded-lg"
                  }`}
                >
                  <div className="min-w-0 flex-1 basis-[24rem]">
                    <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
                      <CategoryBadge category={item.category_id} />
                      {item.ai_suggested ? (
                        <StatusBadge
                          tone="ai"
                          title="Suggested by AI — accepting confirms the sorting is correct"
                        >
                          AI
                        </StatusBadge>
                      ) : null}
                      {item.source_user_id ? (
                        <StatusBadge
                          tone="info"
                          title="Which connected account/input this item came from — email replies are drafted in that account's mailbox"
                        >
                          {userNames.get(item.source_user_id) ??
                            item.source_user_id}
                        </StatusBadge>
                      ) : null}
                      {item.assignee_user_id ? (
                        <StatusBadge
                          tone="neutral"
                          title="Operator currently assigned to this work item"
                        >
                          {userNames.get(item.assignee_user_id) ??
                            item.assignee_user_id}
                        </StatusBadge>
                      ) : itemSharedVisible ? (
                        <StatusBadge
                          tone="warning"
                          title="Shared-visible item with no current assignee"
                        >
                          Unassigned
                        </StatusBadge>
                      ) : null}
                      {entry.attention ? (
                        <StatusBadge
                          tone={attentionTone(entry.attention.level)}
                          title={attentionTitle(entry)}
                        >
                          {attentionDisplayLabel(entry.attention.label)}
                        </StatusBadge>
                      ) : null}
                      <span className="min-w-[12rem] flex-1 truncate text-sm font-semibold text-zinc-100">
                        {item.title || "(untitled)"}
                      </span>
                      <OutputsControl
                        kinds={item.packet_kinds}
                        catalog={packetKindsById}
                        editable={!busy && item.status !== "dismissed"}
                        onChange={(kinds) => void updateKinds(entry, kinds)}
                      />
                      {stagedKinds.length > 0 ? (
                        <StatusBadge
                          tone="warning"
                          title={`Staged draft awaiting your decision: ${stagedKinds
                            .map((k) => packetKindsById.get(k)?.title ?? k)
                            .join(", ")}`}
                        >
                          draft ready
                        </StatusBadge>
                      ) : null}
                      {pendingKinds.length > 0 ? (
                        <StatusBadge
                          tone="progress"
                          pulse
                          title={`AI is preparing: ${pendingKinds
                            .map((k) => packetKindsById.get(k)?.title ?? k)
                            .join(", ")} — it will show as "draft ready" when done, no action needed yet`}
                        >
                          drafting…
                        </StatusBadge>
                      ) : null}
                      {failureNotifications.length > 0 ? (
                        <StatusBadge
                          tone="critical"
                          title={failureNotifications
                            .map((f) => {
                              const kind = f.packet_kind
                                ? (packetKindsById.get(f.packet_kind)?.title ??
                                  f.packet_kind)
                                : null;
                              return `${f.title}${kind ? `: ${kind}` : ""}. ${
                                f.next_action ?? f.message
                              }`;
                            })
                            .join(" ")}
                        >
                          {failureNotifications.length === 1
                            ? "task failed"
                            : `${failureNotifications.length} task failures`}
                        </StatusBadge>
                      ) : null}
                      {item.produce_guidance ? (
                        <StatusBadge tone="neutral" title={item.produce_guidance}>
                          context
                        </StatusBadge>
                      ) : null}
                      {debugEnabled && firstDebugNotification?.diagnostic_id ? (
                        <Button
                          variant="ghost"
                          size="sm"
                          onClick={(event) => {
                            event.stopPropagation();
                            onOpenDebug(
                              firstDebugNotification.diagnostic_id ?? undefined,
                            );
                          }}
                          title="Open the matching diagnostics row"
                        >
                          Debug
                        </Button>
                      ) : null}
                    </div>
                    {item.summary ? (
                      <div className="mt-0.5 truncate text-xs text-zinc-400">
                        {item.summary}
                      </div>
                    ) : null}
                    {item.ai_suggested && item.rationale ? (
                      <div className="mt-0.5 truncate text-xs italic text-violet-300/80">
                        "{item.rationale}"
                      </div>
                    ) : null}
                  </div>
                  <span
                    className="ml-auto shrink-0 whitespace-nowrap pt-0.5 text-xs text-zinc-400"
                    title={new Date(item.created_at_ms).toLocaleString()}
                  >
                    {relativeAge(item.created_at_ms)}
                  </span>
                  <div className="flex w-full shrink-0 items-start justify-end gap-2 pt-0.5 sm:w-auto">
                    {canAssignToMe ? (
                      <Button
                        variant="secondary"
                        size="sm"
                        onClick={() => void updateAssignment(entry, "assign_to_me")}
                        busy={busy}
                        title="Assign this item to yourself"
                      >
                        Assign to me
                      </Button>
                    ) : null}
                    {canUnassign ? (
                      <Button
                        variant="secondary"
                        size="sm"
                        onClick={() => void updateAssignment(entry, "unassign")}
                        busy={busy}
                        title="Clear the current assignee"
                      >
                        Unassign
                      </Button>
                    ) : null}
                    <Button
                      variant="secondary"
                      size="sm"
                      onClick={() =>
                        setSourceItemId(sourceOpen ? null : item.item_id)
                      }
                      title="Read the full source email/note inline"
                    >
                      {sourceOpen ? "Hide source" : "Source"}
                    </Button>
                    <Button
                      variant="secondary"
                      size="sm"
                      onClick={() =>
                        setGuidanceItemId(
                          guidanceItemId === item.item_id ? null : item.item_id,
                        )
                      }
                      title="Add context for AI-generated drafts"
                    >
                      Context
                    </Button>
                    {agentLaunchEnabled ? (
                      <Button
                        variant="secondary"
                        size="sm"
                        onClick={() =>
                          setLaunchItemId(
                            launchItemId === item.item_id ? null : item.item_id,
                          )
                        }
                        title="Open an agent session seeded with this item's context"
                      >
                        Launch agent
                      </Button>
                    ) : null}
                    {item.status === "open" ? (
                      <>
                        {hasProduceKind ? (
                          <Button
                            variant="success"
                            size="sm"
                            onClick={() => void runAction(entry, "accept")}
                            busy={busy}
                            title="Accept and prepare selected outputs"
                          >
                            Accept
                          </Button>
                        ) : (
                          <span
                            className="text-xs italic text-zinc-400"
                            title="Add an output with Edit before accepting, or dismiss if no action is needed"
                          >
                            No outputs
                          </span>
                        )}
                        <Button
                          variant="secondary"
                          size="sm"
                          onClick={() => void runAction(entry, "dismiss")}
                          busy={busy}
                        >
                          Dismiss
                        </Button>
                        {item.source_kind === "email" ? (
                          <Button
                            variant="danger"
                            size="sm"
                            onClick={() => void runAction(entry, "trash")}
                            busy={busy}
                            title="Move the source message to Gmail Trash and dismiss this item"
                          >
                            Trash email
                          </Button>
                        ) : null}
                      </>
                    ) : item.status === "accepted" ? (
                      <>
                        {hasProduceKind ? (
                          <Button
                            variant={expanded ? "secondary" : "success"}
                            size="sm"
                            onClick={() => {
                              if (hasStagedDraft) {
                                setExpandedItemId(expanded ? null : item.item_id);
                                if (!expanded) setFollowedItemId(item.item_id);
                                return;
                              }
                              if (expanded && missingDraftKinds.length === 0) {
                                setExpandedItemId(null);
                                return;
                              }
                              void produceMissingDrafts(entry);
                            }}
                            busy={busy}
                            title={
                              hasStagedDraft
                                ? "Open the staged drafts awaiting review"
                                : "Create drafts for the selected outputs; edit the output chips first if needed"
                            }
                          >
                            {hasStagedDraft
                              ? expanded
                                ? "Hide drafts"
                                : "Open drafts"
                              : busy && missingDraftKinds.length > 0
                              ? "Creating drafts"
                              : expanded && missingDraftKinds.length === 0
                                ? "Hide drafts"
                                : missingDraftKinds.length > 0
                                  ? "Create drafts"
                                  : "Drafts"}
                          </Button>
                        ) : (
                          <span className="text-xs italic text-zinc-400">
                            drafts not available for this type yet
                          </span>
                        )}
                        <Button
                          variant="secondary"
                          size="sm"
                          onClick={() => void runAction(entry, "reopen")}
                          busy={busy}
                        >
                          Reopen
                        </Button>
                        <Button
                          variant="secondary"
                          size="sm"
                          onClick={() => void runAction(entry, "dismiss")}
                          busy={busy}
                          title="Dismiss this item without reopening it"
                        >
                          Dismiss
                        </Button>
                        {item.source_kind === "email" ? (
                          <Button
                            variant="danger"
                            size="sm"
                            onClick={() => void runAction(entry, "trash")}
                            busy={busy}
                            title="Move the source message to Gmail Trash and dismiss this item"
                          >
                            Trash email
                          </Button>
                        ) : null}
                      </>
                    ) : (
                      <Button
                        variant="secondary"
                        size="sm"
                        onClick={() => void runAction(entry, "reopen")}
                        busy={busy}
                      >
                        Reopen
                      </Button>
                    )}
                  </div>
                </div>
              {sourceOpen ? (
                <SourcePeek
                  itemId={item.item_id}
                  onUnauthorized={onUnauthorized}
                />
              ) : null}
              {guidanceItemId === item.item_id ? (
                <ProduceGuidanceEditor
                  entry={entry}
                  busy={busy}
                  onSave={updateGuidance}
                  onUnauthorized={onUnauthorized}
                />
              ) : null}
              {agentLaunchEnabled && launchItemId === item.item_id ? (
                <LaunchAgentPanel
                  itemId={item.item_id}
                  defaultContext={
                    categoriesById.get(item.category_id)?.default_agent_context ?? ""
                  }
                  defaultWorkDir={
                    categoriesById.get(item.category_id)?.default_agent_dir ||
                    AGENT_WORK_DIR_FALLBACK
                  }
                  onUnauthorized={onUnauthorized}
                />
              ) : null}
              {expanded && item.status === "accepted" ? (
                <ItemDraftTabs
                  itemId={item.item_id}
                  kinds={draftKinds}
                  stagedKinds={stagedKinds}
                  pendingKinds={pendingKinds}
                  onUnauthorized={onUnauthorized}
                />
              ) : null}
              </div>
            );
          })}
          </div>
        </div>
      ) : null}
    </div>
  );
}
