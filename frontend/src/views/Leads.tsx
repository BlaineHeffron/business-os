import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type { LeadDiscoverySourceConfig } from "../types/generated/LeadDiscoverySourceConfig";
import type { LeadDiscoverySourceKind } from "../types/generated/LeadDiscoverySourceKind";
import type { LeadDiscoveryStatusResponse } from "../types/generated/LeadDiscoveryStatusResponse";
import type { LeadFindingStatus } from "../types/generated/LeadFindingStatus";
import type { LeadFindingWithRevision } from "../types/generated/LeadFindingWithRevision";
import { api, errorMessage, isUnauthorized } from "../lib/api";
import { Button, EmptyState, SkeletonRows, StatusBadge, Surface } from "../components/ui";

type Notice = { text: string; kind: "info" | "error" };

interface ManualLeadDraft {
  source_id: string;
  title: string;
  summary: string;
  evidence_quote: string;
  item_url: string;
  contact_hint: string;
  company_hint: string;
}

const FILTERS: { id: LeadFindingStatus | "all"; label: string }[] = [
  { id: "staged", label: "Needs review" },
  { id: "accepted", label: "Accepted" },
  { id: "rejected", label: "Dismissed" },
  { id: "all", label: "All" },
];

const SOURCE_KIND_LABELS: Record<LeadDiscoverySourceKind, string> = {
  forum: "Forum",
  reddit: "Reddit",
  google_alert: "Google Alert",
  facebook_group: "Facebook group",
  other: "Other source",
};

const FINDING_STATUS_LABELS: Record<LeadFindingStatus, string> = {
  staged: "Needs review",
  accepted: "Accepted",
  rejected: "Dismissed",
};

function sourceTone(approved: boolean, enabled: boolean) {
  if (approved && enabled) return "ok" as const;
  if (approved) return "warning" as const;
  return "neutral" as const;
}

function sourceStateLabel(source: LeadDiscoverySourceConfig) {
  if (!source.approved) return "Pending";
  if (source.enabled) return "Enabled";
  return "Approved";
}

function isLiveSource(source: LeadDiscoverySourceConfig) {
  return source.approved && source.enabled;
}

function findingTone(status: LeadFindingStatus) {
  if (status === "accepted") return "ok" as const;
  if (status === "rejected") return "neutral" as const;
  return "warning" as const;
}

function formatDate(ms: number | null | undefined): string {
  if (!ms) return "Unknown date";
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  }).format(new Date(ms));
}

function formatLastChecked(ms: number | null | undefined): string {
  if (!ms) return "Not checked yet";
  return `Last checked ${formatDate(ms)}`;
}

function emptyDraft(sourceId = ""): ManualLeadDraft {
  return {
    source_id: sourceId,
    title: "",
    summary: "",
    evidence_quote: "",
    item_url: "",
    contact_hint: "",
    company_hint: "",
  };
}

function nullableTrim(value: string): string | null {
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}

function matchingTerms(draft: ManualLeadDraft, status: LeadDiscoveryStatusResponse | null) {
  const haystack = `${draft.title} ${draft.summary} ${draft.evidence_quote}`.toLowerCase();
  const terms = [
    ...(status?.criteria.lead_markets ?? []),
    ...(status?.criteria.intent_terms ?? []),
  ];
  return Array.from(
    new Set(terms.filter((term) => haystack.includes(term.toLowerCase()))),
  );
}

function LabelChips({ labels }: { labels: string[] }) {
  if (labels.length === 0) return null;
  return (
    <span className="inline-flex flex-wrap gap-1 align-middle">
      {labels.map((label) => (
        <span
          key={label}
          className="rounded bg-zinc-800 px-1.5 py-0.5 text-xs font-medium text-zinc-300 ring-1 ring-inset ring-zinc-700"
        >
          {label}
        </span>
      ))}
    </span>
  );
}

function FieldLabel({
  label,
  children,
}: {
  label: string;
  children: ReactNode;
}) {
  return (
    <label className="block">
      <span className="text-xs font-medium text-zinc-400">{label}</span>
      <div className="mt-1">{children}</div>
    </label>
  );
}

function textInputCls(invalid = false) {
  return [
    "w-full rounded-md border bg-zinc-950/60 px-3 py-2 text-sm text-zinc-100 outline-none",
    "placeholder:text-zinc-600 focus:border-sky-600 focus:ring-1 focus:ring-sky-600",
    invalid ? "border-red-700" : "border-zinc-700",
  ].join(" ");
}

function ManualLeadModal({
  open,
  status,
  draft,
  busy,
  error,
  onDraft,
  onCancel,
  onSubmit,
}: {
  open: boolean;
  status: LeadDiscoveryStatusResponse | null;
  draft: ManualLeadDraft;
  busy: boolean;
  error: string | null;
  onDraft: (draft: ManualLeadDraft) => void;
  onCancel: () => void;
  onSubmit: () => void;
}) {
  const cancelRef = useRef<HTMLButtonElement>(null);
  const enabledSources = useMemo(
    () => status?.sources.filter(isLiveSource) ?? [],
    [status],
  );

  useEffect(() => {
    if (!open) return;
    cancelRef.current?.focus({ preventScroll: true });
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open, onCancel]);

  if (!open) return null;

  const requiredMissing =
    draft.source_id.trim().length === 0 ||
    draft.title.trim().length === 0 ||
    draft.summary.trim().length === 0 ||
    draft.evidence_quote.trim().length === 0;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onCancel();
      }}
    >
      <div className="max-h-[calc(100dvh-2rem)] w-full max-w-2xl overflow-y-auto rounded-lg border border-zinc-700 bg-zinc-900 p-5 shadow-xl">
        <div className="flex items-start justify-between gap-3">
          <div>
            <h3 className="text-sm font-semibold text-zinc-100">Log a lead</h3>
            <p className="mt-1 text-sm text-zinc-400">
              Stage one approved-source finding for review. Accept still happens from the list.
            </p>
          </div>
          <StatusBadge tone="warning">Manual</StatusBadge>
        </div>

        <div className="mt-4 grid gap-3 sm:grid-cols-2">
          <FieldLabel label="Source">
            <select
              value={draft.source_id}
              onChange={(e) => onDraft({ ...draft, source_id: e.target.value })}
              className={textInputCls(draft.source_id.trim().length === 0)}
              disabled={busy}
            >
              <option value="">Choose a source</option>
              {enabledSources.map((source) => (
                <option key={source.source_id} value={source.source_id}>
                  {source.display_name}
                </option>
              ))}
            </select>
          </FieldLabel>
          <FieldLabel label="Link">
            <input
              value={draft.item_url}
              onChange={(e) => onDraft({ ...draft, item_url: e.target.value })}
              className={textInputCls()}
              placeholder="https://..."
              disabled={busy}
            />
          </FieldLabel>
          <FieldLabel label="Title">
            <input
              value={draft.title}
              onChange={(e) => onDraft({ ...draft, title: e.target.value })}
              className={textInputCls(draft.title.trim().length === 0)}
              disabled={busy}
            />
          </FieldLabel>
          <FieldLabel label="Company">
            <input
              value={draft.company_hint}
              onChange={(e) => onDraft({ ...draft, company_hint: e.target.value })}
              className={textInputCls()}
              disabled={busy}
            />
          </FieldLabel>
          <FieldLabel label="Contact">
            <input
              value={draft.contact_hint}
              onChange={(e) => onDraft({ ...draft, contact_hint: e.target.value })}
              className={textInputCls()}
              disabled={busy}
            />
          </FieldLabel>
          <div className="sm:col-span-2">
            <FieldLabel label="Summary">
              <textarea
                value={draft.summary}
                onChange={(e) => onDraft({ ...draft, summary: e.target.value })}
                className={`${textInputCls(draft.summary.trim().length === 0)} min-h-20 resize-y`}
                disabled={busy}
              />
            </FieldLabel>
          </div>
          <div className="sm:col-span-2">
            <FieldLabel label="What they said">
              <textarea
                value={draft.evidence_quote}
                onChange={(e) =>
                  onDraft({ ...draft, evidence_quote: e.target.value })
                }
                className={`${textInputCls(draft.evidence_quote.trim().length === 0)} min-h-24 resize-y`}
                disabled={busy}
              />
            </FieldLabel>
          </div>
        </div>

        {error ? (
          <div className="mt-4 rounded-md border border-red-800 bg-red-950/40 px-3 py-2 text-sm text-red-200">
            {error}
          </div>
        ) : null}

        <div className="mt-4 flex flex-wrap items-center justify-between gap-3">
          <div className="text-xs text-zinc-500">
            {enabledSources.length === 0
              ? "No enabled sources are available."
              : "Required: source, title, summary, and evidence quote."}
          </div>
          <div className="flex justify-end gap-2">
            <Button ref={cancelRef} variant="secondary" size="sm" onClick={onCancel}>
              Cancel
            </Button>
            <Button
              variant="primary"
              size="sm"
              busy={busy}
              disabled={requiredMissing || enabledSources.length === 0}
              onClick={onSubmit}
            >
              {busy ? "Logging..." : "Log lead"}
            </Button>
          </div>
        </div>
      </div>
    </div>
  );
}

export default function Leads({
  onUnauthorized,
  onOpenQueue,
}: {
  onUnauthorized: () => void;
  onOpenQueue?: (itemId: string) => void;
}) {
  const [status, setStatus] = useState<LeadDiscoveryStatusResponse | null>(null);
  const [findings, setFindings] = useState<LeadFindingWithRevision[]>([]);
  const [filter, setFilter] = useState<LeadFindingStatus | "all">("staged");
  const [loading, setLoading] = useState(true);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [expandedFindingId, setExpandedFindingId] = useState<string | null>(null);
  const [sourcesOpen, setSourcesOpen] = useState(false);
  const [modalOpen, setModalOpen] = useState(false);
  const [manualDraft, setManualDraft] = useState<ManualLeadDraft>(() => emptyDraft());
  const [manualBusy, setManualBusy] = useState(false);
  const [manualError, setManualError] = useState<string | null>(null);

  const enabledSources = useMemo(
    () => status?.sources.filter(isLiveSource) ?? [],
    [status],
  );
  const autoSources = useMemo(
    () => status?.sources.filter((source) => source.auto_poll) ?? [],
    [status],
  );
  const manualSources = useMemo(
    () => status?.sources.filter((source) => !source.auto_poll) ?? [],
    [status],
  );

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const [statusRes, findingsRes] = await Promise.all([
        api.leadDiscoveryStatus(),
        api.leadFindings(filter === "all" ? undefined : filter),
      ]);
      setStatus(statusRes);
      setFindings(findingsRes.findings);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      setNotice({ text: errorMessage(err), kind: "error" });
    } finally {
      setLoading(false);
    }
  }, [filter, onUnauthorized]);

  useEffect(() => {
    void load();
  }, [load]);

  const openManualModal = (sourceId?: string) => {
    setManualDraft(emptyDraft(sourceId ?? enabledSources[0]?.source_id ?? ""));
    setManualError(null);
    setNotice(null);
    setModalOpen(true);
  };

  const act = async (
    finding: LeadFindingWithRevision,
    action: "accept" | "reject",
  ) => {
    setBusyId(finding.finding.finding_id);
    setNotice(null);
    try {
      await api.leadFindingAction(finding.finding.finding_id, {
        action,
        expected_revision: finding.revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNotice({
        text: action === "accept" ? "Lead accepted and sent to Queue." : "Lead dismissed.",
        kind: "info",
      });
      await load();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      setNotice({ text: errorMessage(err), kind: "error" });
    } finally {
      setBusyId(null);
    }
  };

  const submitManualLead = async () => {
    setManualBusy(true);
    setManualError(null);
    setNotice(null);
    try {
      await api.leadFindingStage({
        source_id: manualDraft.source_id,
        title: manualDraft.title.trim(),
        summary: manualDraft.summary.trim(),
        evidence_quote: manualDraft.evidence_quote.trim(),
        item_url: nullableTrim(manualDraft.item_url),
        contact_hint: nullableTrim(manualDraft.contact_hint),
        company_hint: nullableTrim(manualDraft.company_hint),
        matched_terms: matchingTerms(manualDraft, status),
        captured_at_ms: null,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setModalOpen(false);
      setNotice({ text: "Lead logged for review.", kind: "info" });
      await load();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      setManualError(errorMessage(err));
    } finally {
      setManualBusy(false);
    }
  };

  return (
    <div className="text-zinc-100">
      <div className="max-w-6xl">
        <div className="mb-4 flex flex-wrap items-start justify-between gap-3">
          <div>
            <h1 className="text-lg font-semibold tracking-normal text-zinc-100">
              Leads
            </h1>
            <p className="mt-1 max-w-2xl text-sm text-zinc-400">
              Approved-source findings for operator review. Outreach is never automated.
            </p>
          </div>
          <div className="flex gap-2">
            <Button variant="primary" onClick={() => openManualModal()}>
              Log a lead
            </Button>
            <Button variant="secondary" onClick={() => void load()}>
              Refresh
            </Button>
          </div>
        </div>

        {notice ? (
          <div
            className={`mb-4 rounded-md border px-3 py-2 text-sm ${
              notice.kind === "info"
                ? "border-sky-800 bg-sky-950/40 text-sky-200"
                : "border-red-800 bg-red-950/40 text-red-200"
            }`}
          >
            {notice.text}
          </div>
        ) : null}

        <section className="mb-5">
          <div className="mb-3 flex flex-wrap items-center justify-between gap-3">
            <div className="flex flex-wrap gap-2">
              {FILTERS.map((entry) => (
                <Button
                  key={entry.id}
                  variant={filter === entry.id ? "primary" : "ghost"}
                  size="sm"
                  onClick={() => setFilter(entry.id)}
                >
                  {entry.label}
                </Button>
              ))}
            </div>
            {status ? (
              <div className="flex flex-wrap items-center gap-2 text-xs text-zinc-500">
                <StatusBadge tone={status.configured ? "ok" : "warning"}>
                  {status.configured ? "Sources ready" : "Sources pending"}
                </StatusBadge>
                <span>
                  {status.enabled_sources} enabled · {status.pending_sources} pending
                </span>
              </div>
            ) : null}
          </div>

          {loading ? (
            <SkeletonRows rows={6} />
          ) : findings.length === 0 ? (
            <EmptyState title="No findings">
              Findings appear here after an approved source stages one with evidence.
            </EmptyState>
          ) : (
            <div className="surface-card surface-flat surface-body-orange surface-row-divide divide-y divide-zinc-800 overflow-hidden rounded-lg border border-zinc-800">
              {findings.map((entry) => {
                const finding = entry.finding;
                const staged = finding.status === "staged";
                const expanded = expandedFindingId === finding.finding_id;
                return (
                  <article key={finding.finding_id} className="px-4 py-4">
                    <div className="flex flex-wrap items-start justify-between gap-3">
                      <div
                        role="button"
                        tabIndex={0}
                        className="min-w-0 flex-1 cursor-pointer rounded-md text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70"
                        onClick={() =>
                          setExpandedFindingId(expanded ? null : finding.finding_id)
                        }
                        onKeyDown={(event) => {
                          if (event.key !== "Enter" && event.key !== " ") return;
                          event.preventDefault();
                          setExpandedFindingId(expanded ? null : finding.finding_id);
                        }}
                      >
                        <div className="mb-1 flex flex-wrap items-center gap-2">
                          <StatusBadge tone={findingTone(finding.status)}>
                            {FINDING_STATUS_LABELS[finding.status]}
                          </StatusBadge>
                          <span className="text-xs text-zinc-500">
                            {finding.evidence.source.display_name} ·{" "}
                            {formatDate(finding.evidence.captured_at_ms)}
                          </span>
                        </div>
                        <h2 className="text-sm font-semibold text-zinc-100">
                          {finding.title}
                        </h2>
                        <p className="mt-1 text-sm leading-6 text-zinc-300">
                          {finding.summary}
                        </p>
                      </div>
                      {staged ? (
                        <div className="flex gap-2">
                          <Button
                            size="sm"
                            variant="success"
                            busy={busyId === finding.finding_id}
                            onClick={() => void act(entry, "accept")}
                          >
                            Accept
                          </Button>
                          <Button
                            size="sm"
                            variant="ghost"
                            busy={busyId === finding.finding_id}
                            onClick={() => void act(entry, "reject")}
                          >
                            Reject
                          </Button>
                        </div>
                      ) : null}
                    </div>

                    {expanded ? (
                      <div className="mt-4 grid gap-3 md:grid-cols-2">
                        <div className="space-y-2 text-xs text-zinc-400">
                          {finding.company_hint ? (
                            <div>
                              <span className="text-zinc-500">Company:</span>{" "}
                              {finding.company_hint}
                            </div>
                          ) : null}
                          {finding.contact_hint ? (
                            <div>
                              <span className="text-zinc-500">Contact:</span>{" "}
                              {finding.contact_hint}
                            </div>
                          ) : null}
                          {finding.matched_terms.length > 0 ? (
                            <div className="flex flex-wrap items-center gap-2">
                              <span className="text-zinc-500">Matched:</span>
                              <LabelChips labels={finding.matched_terms} />
                            </div>
                          ) : null}
                          {finding.work_item_id ? (
                            <button
                              type="button"
                              className="text-sky-300 hover:text-sky-200"
                              onClick={() => onOpenQueue?.(finding.work_item_id ?? "")}
                            >
                              View in Queue →
                            </button>
                          ) : null}
                        </div>
                        <blockquote className="rounded-md border border-zinc-800 bg-zinc-900/40 px-3 py-2 text-xs leading-5 text-zinc-300">
                          {finding.evidence.evidence_quote}
                          {finding.evidence.item_url ? (
                            <a
                              className="mt-2 block truncate text-sky-300 hover:text-sky-200"
                              href={finding.evidence.item_url}
                              target="_blank"
                              rel="noreferrer"
                            >
                              Evidence link ↗
                            </a>
                          ) : null}
                        </blockquote>
                      </div>
                    ) : null}
                  </article>
                );
              })}
            </div>
          )}
        </section>

        <Surface
          accent="orange"
          title="Sources"
          subtitle="Approved places the team watches. Facebook groups stay manual-only."
          actions={
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setSourcesOpen((open) => !open)}
            >
              {sourcesOpen ? "Hide" : "Show"}
            </Button>
          }
        >
          {status ? (
            <div className="space-y-4">
              <div className="grid gap-3 md:grid-cols-2">
                <div>
                  <div className="mb-2 text-xs font-semibold uppercase tracking-wide text-zinc-500">
                    Watching for:
                  </div>
                  {status.criteria.lead_markets.length > 0 ? (
                    <LabelChips labels={status.criteria.lead_markets} />
                  ) : (
                    <span className="text-sm text-zinc-500">
                      No lead markets configured.
                    </span>
                  )}
                </div>
                <div>
                  <div className="mb-2 text-xs font-semibold uppercase tracking-wide text-zinc-500">
                    Buying signals:
                  </div>
                  {status.criteria.intent_terms.length > 0 ? (
                    <LabelChips labels={status.criteria.intent_terms} />
                  ) : (
                    <span className="text-sm text-zinc-500">
                      No buying signals configured.
                    </span>
                  )}
                </div>
              </div>

              {sourcesOpen ? (
                <div className="space-y-5">
                  <div>
                    <div className="mb-2 flex items-center gap-2">
                      <h2 className="text-xs font-semibold uppercase tracking-wide text-zinc-500">
                        Auto
                      </h2>
                      <StatusBadge tone="warning">Feed poller</StatusBadge>
                    </div>
                    {autoSources.length > 0 ? (
                      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
                        {autoSources.map((source) => (
                          <div
                            key={source.source_id}
                            className="rounded-md border border-zinc-800 px-3 py-3"
                          >
                            <div className="flex items-start justify-between gap-2">
                              <div className="min-w-0">
                                <div className="truncate text-sm font-medium text-zinc-200">
                                  {source.display_name}
                                </div>
                                <div className="mt-1 text-xs text-zinc-500">
                                  {SOURCE_KIND_LABELS[source.kind]}
                                </div>
                              </div>
                              <StatusBadge
                                tone={sourceTone(source.approved, source.enabled)}
                              >
                                {sourceStateLabel(source)}
                              </StatusBadge>
                            </div>
                            <div className="mt-3 flex flex-wrap items-center gap-2">
                              <StatusBadge tone="warning">Auto</StatusBadge>
                              <span className="text-xs text-zinc-500">
                                {formatLastChecked(
                                  status.auto_poll_last_checked_at_ms,
                                )}
                              </span>
                            </div>
                            {source.tags.length > 0 ? (
                              <div className="mt-2">
                                <LabelChips labels={source.tags} />
                              </div>
                            ) : null}
                            <div className="mt-3 flex flex-wrap gap-2">
                              {source.url ? (
                                <a
                                  href={source.url}
                                  target="_blank"
                                  rel="noreferrer"
                                  className="inline-flex items-center rounded-md border border-zinc-700 px-2.5 py-1 text-xs font-medium text-zinc-300 hover:bg-zinc-800 hover:text-zinc-100"
                                >
                                  Open ↗
                                </a>
                              ) : null}
                              {isLiveSource(source) ? (
                                <Button
                                  size="sm"
                                  variant="secondary"
                                  onClick={() => openManualModal(source.source_id)}
                                >
                                  Log a lead
                                </Button>
                              ) : null}
                            </div>
                          </div>
                        ))}
                      </div>
                    ) : (
                      <div className="rounded-md border border-dashed border-zinc-800 px-3 py-3 text-sm text-zinc-500">
                        No auto-polled feed sources are configured.
                      </div>
                    )}
                  </div>

                  <div>
                    <div className="mb-2 flex items-center gap-2">
                      <h2 className="text-xs font-semibold uppercase tracking-wide text-zinc-500">
                        Manual
                      </h2>
                      <StatusBadge tone="neutral">Operator logged</StatusBadge>
                    </div>
                    {manualSources.length > 0 ? (
                      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
                        {manualSources.map((source) => (
                          <div
                            key={source.source_id}
                            className="rounded-md border border-zinc-800 px-3 py-3"
                          >
                            <div className="flex items-start justify-between gap-2">
                              <div className="min-w-0">
                                <div className="truncate text-sm font-medium text-zinc-200">
                                  {source.display_name}
                                </div>
                                <div className="mt-1 text-xs text-zinc-500">
                                  {SOURCE_KIND_LABELS[source.kind]}
                                </div>
                              </div>
                              <StatusBadge
                                tone={sourceTone(source.approved, source.enabled)}
                              >
                                {sourceStateLabel(source)}
                              </StatusBadge>
                            </div>
                            {source.tags.length > 0 ? (
                              <div className="mt-2">
                                <LabelChips labels={source.tags} />
                              </div>
                            ) : null}
                            <div className="mt-3 flex flex-wrap gap-2">
                              {source.url ? (
                                <a
                                  href={source.url}
                                  target="_blank"
                                  rel="noreferrer"
                                  className="inline-flex items-center rounded-md border border-zinc-700 px-2.5 py-1 text-xs font-medium text-zinc-300 hover:bg-zinc-800 hover:text-zinc-100"
                                >
                                  Open ↗
                                </a>
                              ) : null}
                              {isLiveSource(source) ? (
                                <Button
                                  size="sm"
                                  variant="secondary"
                                  onClick={() => openManualModal(source.source_id)}
                                >
                                  Log a lead
                                </Button>
                              ) : null}
                            </div>
                          </div>
                        ))}
                      </div>
                    ) : (
                      <div className="rounded-md border border-dashed border-zinc-800 px-3 py-3 text-sm text-zinc-500">
                        No manual sources are configured yet.
                      </div>
                    )}
                  </div>
                </div>
              ) : null}
            </div>
          ) : loading ? (
            <SkeletonRows rows={2} />
          ) : null}
        </Surface>
      </div>

      <ManualLeadModal
        open={modalOpen}
        status={status}
        draft={manualDraft}
        busy={manualBusy}
        error={manualError}
        onDraft={(draft) => {
          setManualError(null);
          setManualDraft(draft);
        }}
        onCancel={() => {
          setManualError(null);
          setModalOpen(false);
        }}
        onSubmit={() => void submitManualLead()}
      />
    </div>
  );
}
