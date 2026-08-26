import { useCallback, useEffect, useState } from "react";
import type { HubSpotDealDiscoveryResponse } from "../../types/generated/HubSpotDealDiscoveryResponse";
import type { HubSpotDealMappedStatus } from "../../types/generated/HubSpotDealMappedStatus";
import type { HubSpotDealPipelineMapping } from "../../types/generated/HubSpotDealPipelineMapping";
import type { HubSpotDealPipelineMappingResponse } from "../../types/generated/HubSpotDealPipelineMappingResponse";
import type { HubSpotDealPipelineOption } from "../../types/generated/HubSpotDealPipelineOption";
import { api, errorMessage, isRevisionConflict, isUnauthorized } from "../../lib/api";
import { Button, Card } from "../../components/ui";

const DEAL_STATUS_OPTIONS: { value: HubSpotDealMappedStatus; label: string }[] = [
  { value: "open", label: "Open" },
  { value: "won", label: "Won" },
  { value: "lost", label: "Lost" },
];

function stageStatus(
  mapping: HubSpotDealPipelineMapping | null,
  stageId: string,
): HubSpotDealMappedStatus {
  return mapping?.stage_mappings.find((stage) => stage.stage_id === stageId)?.status ?? "open";
}

function defaultMapping(
  discovery: HubSpotDealDiscoveryResponse,
  current: HubSpotDealPipelineMapping | null,
): HubSpotDealPipelineMapping | null {
  const pipeline =
    discovery.pipelines.find((candidate) => candidate.pipeline_id === current?.pipeline_id) ??
    discovery.pipelines.find((candidate) => !candidate.archived) ??
    discovery.pipelines[0] ??
    null;
  if (!pipeline) return null;
  const dateNames = discovery.date_properties.map((property) => property.name);
  return {
    pipeline_id: pipeline.pipeline_id,
    stage_mappings: pipeline.stages.map((stage) => ({
      stage_id: stage.stage_id,
      label: stage.label,
      status: stageStatus(current, stage.stage_id),
    })),
    started_date_property:
      current?.started_date_property && dateNames.includes(current.started_date_property)
        ? current.started_date_property
        : dateNames.includes("createdate")
          ? "createdate"
          : (dateNames[0] ?? ""),
    closed_date_property:
      current?.closed_date_property && dateNames.includes(current.closed_date_property)
        ? current.closed_date_property
        : dateNames.includes("closedate")
          ? "closedate"
          : (dateNames[0] ?? ""),
  };
}

function selectedPipeline(
  discovery: HubSpotDealDiscoveryResponse | null,
  mapping: HubSpotDealPipelineMapping | null,
): HubSpotDealPipelineOption | null {
  if (!discovery || !mapping) return null;
  return discovery.pipelines.find((pipeline) => pipeline.pipeline_id === mapping.pipeline_id) ?? null;
}

function validateDealMapping(
  discovery: HubSpotDealDiscoveryResponse,
  mapping: HubSpotDealPipelineMapping | null,
): string | null {
  if (!mapping) return "Choose a HubSpot pipeline before saving.";
  const pipeline = selectedPipeline(discovery, mapping);
  if (!pipeline) return "Choose a HubSpot pipeline before saving.";
  const mapped = new Map(mapping.stage_mappings.map((stage) => [stage.stage_id, stage.status]));
  const statuses = pipeline.stages.map((stage) => mapped.get(stage.stage_id) ?? "open");
  if (!statuses.includes("open")) return "Mark at least one stage as open.";
  if (!statuses.includes("won")) return "Mark at least one stage as won.";
  if (!statuses.includes("lost")) return "Mark at least one stage as lost.";
  if (!mapping.started_date_property) return "Choose the started date field.";
  if (!mapping.closed_date_property) return "Choose the closed date field.";
  return null;
}

export function HubSpotDealsSettings({ onUnauthorized }: { onUnauthorized: () => void }) {
  const [discovery, setDiscovery] = useState<HubSpotDealDiscoveryResponse | null>(null);
  const [saved, setSaved] = useState<HubSpotDealPipelineMappingResponse | null>(null);
  const [draft, setDraft] = useState<HubSpotDealPipelineMapping | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ kind: "ok" | "error"; text: string } | null>(
    null,
  );
  const [saving, setSaving] = useState(false);

  const load = useCallback(async () => {
    try {
      const [nextDiscovery, nextSaved] = await Promise.all([
        api.hubSpotDealDiscovery(),
        api.hubSpotDealMapping(),
      ]);
      setDiscovery(nextDiscovery);
      setSaved(nextSaved);
      setDraft(defaultMapping(nextDiscovery, nextSaved.mapping ?? null));
      setError(null);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    }
  }, [onUnauthorized]);

  useEffect(() => {
    void load();
  }, [load]);

  const choosePipeline = (pipelineId: string) => {
    if (!discovery) return;
    const nextPipeline = discovery.pipelines.find((pipeline) => pipeline.pipeline_id === pipelineId);
    if (!nextPipeline) return;
    setDraft((current) => ({
      pipeline_id: nextPipeline.pipeline_id,
      stage_mappings: nextPipeline.stages.map((stage) => ({
        stage_id: stage.stage_id,
        label: stage.label,
        status: stageStatus(current, stage.stage_id),
      })),
      started_date_property: current?.started_date_property ?? discovery.date_properties[0]?.name ?? "",
      closed_date_property: current?.closed_date_property ?? discovery.date_properties[0]?.name ?? "",
    }));
  };

  const setStage = (stageId: string, status: HubSpotDealMappedStatus) => {
    setDraft((current) =>
      current
        ? {
            ...current,
            stage_mappings: current.stage_mappings.map((stage) =>
              stage.stage_id === stageId ? { ...stage, status } : stage,
            ),
          }
        : current,
    );
  };

  const save = async () => {
    if (!discovery) return;
    const validation = validateDealMapping(discovery, draft);
    if (validation) {
      setNotice({ kind: "error", text: validation });
      return;
    }
    setSaving(true);
    setNotice(null);
    try {
      await api.updateHubSpotDealMapping({
        mapping: draft!,
        expected_revision: saved?.revision ?? null,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNotice({ kind: "ok", text: "HubSpot deal mapping saved." });
      await load();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else if (isRevisionConflict(err)) {
        await load();
        setNotice({ kind: "error", text: "Changed elsewhere — reloaded; try again." });
      } else {
        setNotice({ kind: "error", text: `Save failed: ${errorMessage(err)}` });
      }
    } finally {
      setSaving(false);
    }
  };

  if (error) {
    return (
      <div className="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300">
        Failed to load HubSpot deal settings: {error}
      </div>
    );
  }
  if (!discovery || !saved) {
    return <div className="text-sm text-zinc-500">Loading…</div>;
  }
  if (!discovery.configured) {
    return (
      <Card className="surface-flat surface-body-zinc">
        <div className="surface-section-head surface-head-zinc mb-3">
          <div className="text-xs font-semibold uppercase tracking-wide text-zinc-500">
            HubSpot deal pipeline
          </div>
          <div className="mt-1 text-sm text-zinc-300">
            {discovery.message ?? "Connect HubSpot Deals before configuring the dashboard."}
          </div>
        </div>
      </Card>
    );
  }

  const pipeline = selectedPipeline(discovery, draft);

  return (
    <Card className="surface-flat surface-body-zinc">
      <div className="surface-section-head surface-head-zinc mb-3 flex items-center justify-between gap-3">
        <div>
          <div className="text-xs font-semibold uppercase tracking-wide text-zinc-500">
            HubSpot deal pipeline
          </div>
          <div className="mt-1 text-sm text-zinc-300">
            Map HubSpot stages to the Home sales pipeline widget. Deals stay read-only.
          </div>
        </div>
        <Button variant="primary" size="sm" busy={saving} onClick={() => void save()}>
          Save
        </Button>
      </div>
      {notice ? (
        <div
          className={`mb-3 rounded-md border px-3 py-2 text-sm ${
            notice.kind === "ok"
              ? "border-emerald-900/60 bg-emerald-950/30 text-emerald-300"
              : "border-red-900/60 bg-red-950/40 text-red-300"
          }`}
        >
          {notice.text}
        </div>
      ) : null}
      {discovery.message ? (
        <div className="mb-3 rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-2 text-sm text-amber-200">
          {discovery.message}
        </div>
      ) : null}
      <div className="grid gap-3 md:grid-cols-3">
        <label className="text-xs font-medium text-zinc-400 md:col-span-2">
          Pipeline
          <select
            className="mt-1 w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-100 focus:border-sky-600 focus:outline-none"
            value={draft?.pipeline_id ?? ""}
            onChange={(e) => choosePipeline(e.target.value)}
          >
            {discovery.pipelines.map((candidate) => (
              <option key={candidate.pipeline_id} value={candidate.pipeline_id}>
                {candidate.label}
                {candidate.archived ? " (archived)" : ""}
              </option>
            ))}
          </select>
        </label>
        <div className="flex items-end">
          {pipeline?.url ? (
            <a
              className="rounded-md border border-zinc-700 px-2.5 py-1 text-xs font-medium text-zinc-300 transition hover:bg-zinc-800"
              href={pipeline.url}
              target="_blank"
              rel="noreferrer"
            >
              Open in HubSpot
            </a>
          ) : null}
        </div>
      </div>
      <div className="mt-3 grid gap-3 md:grid-cols-2">
        <label className="text-xs font-medium text-zinc-400">
          Started date field
          <select
            className="mt-1 w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-100 focus:border-sky-600 focus:outline-none"
            value={draft?.started_date_property ?? ""}
            onChange={(e) =>
              setDraft((current) =>
                current ? { ...current, started_date_property: e.target.value } : current,
              )
            }
          >
            {discovery.date_properties.map((property) => (
              <option key={property.name} value={property.name}>
                {property.label} · {property.name}
              </option>
            ))}
          </select>
        </label>
        <label className="text-xs font-medium text-zinc-400">
          Closed date field
          <select
            className="mt-1 w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-100 focus:border-sky-600 focus:outline-none"
            value={draft?.closed_date_property ?? ""}
            onChange={(e) =>
              setDraft((current) =>
                current ? { ...current, closed_date_property: e.target.value } : current,
              )
            }
          >
            {discovery.date_properties.map((property) => (
              <option key={property.name} value={property.name}>
                {property.label} · {property.name}
              </option>
            ))}
          </select>
        </label>
      </div>
      <div className="mt-4 overflow-hidden rounded-md border border-zinc-800">
        <div className="grid grid-cols-[minmax(0,1fr)_9rem_4rem] gap-2 border-b border-zinc-800 bg-zinc-950/70 px-3 py-2 text-xs font-semibold uppercase tracking-wide text-zinc-500">
          <span>Stage</span>
          <span>Status</span>
          <span>Link</span>
        </div>
        <div className="divide-y divide-zinc-800">
          {pipeline?.stages.map((stage) => (
            <div
              key={stage.stage_id}
              className="grid grid-cols-[minmax(0,1fr)_9rem_4rem] items-center gap-2 px-3 py-2 text-sm"
            >
              <div className="min-w-0">
                <div className="truncate font-medium text-zinc-100">{stage.label}</div>
                <div className="truncate text-xs text-zinc-500">{stage.stage_id}</div>
              </div>
              <select
                className="rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-100 focus:border-sky-600 focus:outline-none"
                value={stageStatus(draft, stage.stage_id)}
                onChange={(e) =>
                  setStage(stage.stage_id, e.target.value as HubSpotDealMappedStatus)
                }
              >
                {DEAL_STATUS_OPTIONS.map((option) => (
                  <option key={option.value} value={option.value}>
                    {option.label}
                  </option>
                ))}
              </select>
              {stage.url ? (
                <a
                  className="text-xs font-medium text-sky-400 hover:text-sky-300"
                  href={stage.url}
                  target="_blank"
                  rel="noreferrer"
                >
                  Open
                </a>
              ) : (
                <span className="text-xs text-zinc-600">-</span>
              )}
            </div>
          )) ?? null}
        </div>
      </div>
    </Card>
  );
}
