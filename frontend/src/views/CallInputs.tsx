import { useCallback, useEffect, useState } from "react";
import type { CallInputStatus } from "../types/generated/CallInputStatus";
import type { CallInputWithRevision } from "../types/generated/CallInputWithRevision";
import type { CallInputsStatusResponse } from "../types/generated/CallInputsStatusResponse";
import { api, errorMessage, isUnauthorized } from "../lib/api";
import { usePacketKinds } from "../lib/packetKinds";
import { Button, EmptyState, SkeletonRows, StatusBadge } from "../components/ui";

const FILTERS: { id: CallInputStatus | "all"; label: string }[] = [
  { id: "staged", label: "Staged" },
  { id: "accepted", label: "Accepted" },
  { id: "rejected", label: "Rejected" },
  { id: "all", label: "All" },
];

const DEFAULT_CALL_PACKET_KINDS = [
  "crm_activity",
  "follow_up_task",
  "calendar_event_draft",
  "email_draft_reply",
];

function sourceTone(source: {
  enabled: boolean;
}) {
  return source.enabled ? "ok" as const : "neutral" as const;
}

function sourceLabel(source: {
  enabled: boolean;
}) {
  return source.enabled ? "Enabled" : "Pending";
}

function inputTone(status: CallInputStatus) {
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

export default function CallInputs({ onUnauthorized }: { onUnauthorized: () => void }) {
  const [status, setStatus] = useState<CallInputsStatusResponse | null>(null);
  const [inputs, setInputs] = useState<CallInputWithRevision[]>([]);
  const [filter, setFilter] = useState<CallInputStatus | "all">("staged");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [packetSelections, setPacketSelections] = useState<Record<string, string[]>>({});
  const packetKinds = usePacketKinds();

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [statusRes, inputsRes] = await Promise.all([
        api.callInputsStatus(),
        api.callInputs(filter === "all" ? undefined : filter),
      ]);
      setStatus(statusRes);
      setInputs(inputsRes.inputs);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }, [filter, onUnauthorized]);

  useEffect(() => {
    void load();
  }, [load]);

  const act = async (
    entry: CallInputWithRevision,
    action: "accept" | "reject",
  ) => {
    setBusyId(entry.input.call_input_id);
    setError(null);
    try {
      await api.callInputAction(entry.input.call_input_id, {
        action,
        packet_kinds:
          action === "accept"
            ? selectedPacketKinds(entry.input.call_input_id, status)
            : [],
        expected_revision: entry.revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      await load();
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setBusyId(null);
    }
  };

  const packetCatalog = packetKinds.length
    ? packetKinds
        .filter((kind) => kind.produce_available)
        .map((kind) => ({ id: kind.kind_id, title: kind.title }))
    : DEFAULT_CALL_PACKET_KINDS.map((id) => ({ id, title: id }));

  function defaultPacketKinds(currentStatus: CallInputsStatusResponse | null): string[] {
    return currentStatus?.routing.packet_kinds.length
      ? currentStatus.routing.packet_kinds
      : DEFAULT_CALL_PACKET_KINDS;
  }

  function selectedPacketKinds(
    callInputId: string,
    currentStatus: CallInputsStatusResponse | null,
  ): string[] {
    return packetSelections[callInputId] ?? defaultPacketKinds(currentStatus);
  }

  function togglePacketKind(callInputId: string, kind: string) {
    setPacketSelections((current) => {
      const existing = current[callInputId] ?? defaultPacketKinds(status);
      const next = existing.includes(kind)
        ? existing.filter((entry) => entry !== kind)
        : [...existing, kind];
      return { ...current, [callInputId]: next };
    });
  }

  return (
    <div className="text-zinc-100">
      <div className="max-w-6xl">
        <div className="mb-4 flex flex-wrap items-start justify-between gap-3">
          <div>
            <h1 className="text-lg font-semibold tracking-normal text-zinc-100">
              Calls
            </h1>
            <p className="mt-1 max-w-2xl text-sm text-zinc-400">
              Selected call transcripts and logs for review. Recordings are provenance only.
            </p>
          </div>
          <Button variant="secondary" onClick={() => void load()}>
            Refresh
          </Button>
        </div>

        {error ? (
          <div className="mb-4 rounded-md border border-red-800 bg-red-950/40 px-3 py-2 text-sm text-red-200">
            {error}
          </div>
        ) : null}

        <section className="surface-card surface-flat surface-body-sky mb-5 border-y border-zinc-800 py-4">
          {status ? (
            <div className="grid gap-4 lg:grid-cols-[1fr_1.2fr]">
              <div>
                <div className="flex flex-wrap items-center gap-2">
                  <StatusBadge tone={status.configured ? "ok" : "warning"}>
                    {status.configured ? "Configured" : "Pending sources"}
                  </StatusBadge>
                  <span className="text-sm text-zinc-400">
                    {status.enabled_sources} enabled · {status.pending_sources} pending
                  </span>
                </div>
                <div className="mt-3 flex flex-wrap gap-2">
                  {status.routing.packet_kinds.map((kind) => (
                    <span
                      key={kind}
                      className="rounded-md border border-zinc-800 px-2 py-1 text-xs text-zinc-300"
                    >
                      {kind}
                    </span>
                  ))}
                  {status.routing.packet_kinds.length === 0 ? (
                    <span className="text-sm text-zinc-500">Default call routing is active.</span>
                  ) : null}
                </div>
              </div>
              <div className="grid gap-2 sm:grid-cols-2">
                {status.sources.map((source) => (
                  <div
                    key={source.source_id}
                    className="rounded-md border border-zinc-800 px-3 py-2"
                  >
                    <div className="flex items-center justify-between gap-2">
                      <span className="truncate text-sm font-medium text-zinc-200">
                        {source.display_name}
                      </span>
                      <StatusBadge tone={sourceTone(source)}>
                        {sourceLabel(source)}
                      </StatusBadge>
                    </div>
                    <div className="mt-1 truncate text-xs text-zinc-500">
                      {source.kind}
                      {source.location_hint ? ` · ${source.location_hint}` : ""}
                    </div>
                    {source.consent_basis ? (
                      <div className="mt-1 line-clamp-2 text-xs text-zinc-400">
                        {source.consent_basis}
                      </div>
                    ) : null}
                  </div>
                ))}
                {status.sources.length === 0 ? (
                  <div className="rounded-md border border-dashed border-zinc-800 px-3 py-3 text-sm text-zinc-500">
                    No call sources are configured yet.
                  </div>
                ) : null}
              </div>
            </div>
          ) : loading ? (
            <SkeletonRows rows={2} />
          ) : null}
        </section>

        <div className="surface-section-head surface-head-sky mb-3">
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
        </div>

        {loading ? (
          <SkeletonRows rows={6} />
        ) : inputs.length === 0 ? (
          <EmptyState title="No call inputs">
            Call inputs appear here only after an enabled source stages one.
          </EmptyState>
        ) : (
          <div className="surface-card surface-flat surface-body-sky surface-row-divide divide-y divide-zinc-800 border-y border-zinc-800">
            {inputs.map((entry) => {
              const input = entry.input;
              const staged = input.status === "staged";
              return (
                <article key={input.call_input_id} className="py-4">
                  <div className="flex flex-wrap items-start justify-between gap-3">
                    <div className="min-w-0 flex-1">
                      <div className="mb-1 flex flex-wrap items-center gap-2">
                        <StatusBadge tone={inputTone(input.status)}>
                          {input.status}
                        </StatusBadge>
                        <span className="text-xs text-zinc-500">
                          {input.input_kind} · {formatDate(input.occurred_at_ms)}
                        </span>
                      </div>
                      <h2 className="text-sm font-semibold text-zinc-100">
                        {input.title}
                      </h2>
                      <p className="mt-1 text-sm leading-6 text-zinc-300">
                        {input.summary}
                      </p>
                    </div>
                    {staged ? (
                      <div className="flex gap-2">
                        <Button
                          size="sm"
                          variant="success"
                          disabled={selectedPacketKinds(input.call_input_id, status).length === 0}
                          busy={busyId === input.call_input_id}
                          onClick={() => void act(entry, "accept")}
                        >
                          Queue
                        </Button>
                        <Button
                          size="sm"
                          variant="ghost"
                          busy={busyId === input.call_input_id}
                          onClick={() => void act(entry, "reject")}
                        >
                          Reject
                        </Button>
                      </div>
                    ) : null}
                  </div>
                  {staged ? (
                    <div className="mt-3 flex flex-wrap gap-2">
                      {packetCatalog.map((kind) => {
                        const selected = selectedPacketKinds(
                          input.call_input_id,
                          status,
                        ).includes(kind.id);
                        return (
                          <button
                            key={kind.id}
                            type="button"
                            onClick={() => togglePacketKind(input.call_input_id, kind.id)}
                            className={`rounded-md border px-2 py-1 text-xs transition ${
                              selected
                                ? "border-sky-700 bg-sky-950/50 text-sky-100"
                                : "border-zinc-800 text-zinc-400 hover:bg-zinc-900 hover:text-zinc-200"
                            }`}
                          >
                            {kind.title}
                          </button>
                        );
                      })}
                    </div>
                  ) : null}
                  <div className="mt-3 grid gap-3 md:grid-cols-[1fr_1.2fr]">
                    <div className="text-xs text-zinc-500">
                      {input.caller_name ? <div>Caller: {input.caller_name}</div> : null}
                      {input.caller_phone ? <div>Phone: {input.caller_phone}</div> : null}
                      {input.caller_email ? <div>Email: {input.caller_email}</div> : null}
                      {input.work_item_id ? <div>Queue item: {input.work_item_id}</div> : null}
                    </div>
                    <blockquote className="rounded-md border border-zinc-800 bg-zinc-900/40 px-3 py-2 text-xs leading-5 text-zinc-300">
                      {input.recording_ref.evidence_quote}
                      {input.recording_ref.item_url ? (
                        <a
                          className="mt-2 block truncate text-sky-300 hover:text-sky-200"
                          href={input.recording_ref.item_url}
                          target="_blank"
                          rel="noreferrer"
                        >
                          {input.recording_ref.item_url}
                        </a>
                      ) : null}
                    </blockquote>
                  </div>
                </article>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
