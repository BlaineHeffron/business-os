import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  errorMessage,
  isRevisionConflict,
  isUnauthorized,
} from "../lib/api";
import SectionHelpButton from "../components/SectionHelpButton";
import ContentCampaignWorkspace from "../components/ContentCampaignWorkspace";
import { Button, EmptyState, SkeletonList, StatusBadge } from "../components/ui";
import type { ContentCollisionMatch } from "../types/generated/ContentCollisionMatch";
import type { ContentInventoryItemWithRevision } from "../types/generated/ContentInventoryItemWithRevision";
import type { ContentPlanItemWithRevision } from "../types/generated/ContentPlanItemWithRevision";
import type { ContentPlanStatus } from "../types/generated/ContentPlanStatus";

type Notice = { text: string; kind: "error" | "conflict" | "success" } | null;
type Mode = "plans" | "inventory";

const FORM_CONTROL_CLS =
  "w-full rounded-md border border-zinc-700 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 outline-none focus-visible:border-sky-600 focus-visible:ring-2 focus-visible:ring-sky-500/30";

const STATUS_LABEL: Record<ContentPlanStatus, string> = {
  planned: "Planned",
  queued: "Queued",
  published: "Published",
  cancelled: "Cancelled",
};

const REASON_LABEL: Record<string, string> = {
  exact_query: "Same target search",
  same_slug: "Same page or URL",
  similar: "Possible overlap",
};

function planTone(status: ContentPlanStatus): "neutral" | "info" | "ok" | "warning" {
  if (status === "published") return "ok";
  if (status === "queued") return "info";
  if (status === "cancelled") return "warning";
  return "neutral";
}

function draftLabel(entry: ContentPlanItemWithRevision): string {
  if (entry.draft_state === "approved") return "Draft approved";
  if (entry.draft_state === "staged") return "Draft ready";
  if (entry.item.work_item_id) return "In queue";
  return "No draft";
}

function sourceLabel(source: string): string {
  if (source === "plan_item") return "Plan";
  if (source === "search_console_page") return "Analytics";
  if (source === "content_draft") return "Draft";
  return "Manual";
}

function reasonLabel(match: ContentCollisionMatch): string {
  return REASON_LABEL[match.reason] ?? "Possible overlap";
}

export function nextContentPlanId(
  entries: readonly ContentPlanItemWithRevision[],
  currentId: string | null,
  key: "ArrowDown" | "ArrowUp" | "j" | "k" | "Home" | "End",
): string | null {
  if (entries.length === 0) return null;
  if (key === "Home") return entries[0].item.plan_item_id;
  if (key === "End") return entries[entries.length - 1].item.plan_item_id;
  const currentIndex = Math.max(
    0,
    entries.findIndex((entry) => entry.item.plan_item_id === currentId),
  );
  const direction = key === "ArrowDown" || key === "j" ? 1 : -1;
  const nextIndex = (currentIndex + direction + entries.length) % entries.length;
  return entries[nextIndex].item.plan_item_id;
}

export default function ContentPlans({
  onUnauthorized,
  helpTopicId,
  onOpenHelpTopic,
  onOpenQueue,
}: {
  onUnauthorized: () => void;
  helpTopicId?: string;
  onOpenHelpTopic: (topicId: string) => void;
  onOpenQueue: (itemId: string) => void;
}) {
  const [plans, setPlans] = useState<ContentPlanItemWithRevision[]>([]);
  const [inventory, setInventory] = useState<ContentInventoryItemWithRevision[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [mode, setMode] = useState<Mode>("plans");
  const [loaded, setLoaded] = useState(false);
  const [showCreate, setShowCreate] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [notice, setNotice] = useState<Notice>(null);
  const [error, setError] = useState<string | null>(null);
  const [newPlan, setNewPlan] = useState({
    topic: "",
    angle: "",
    format: "",
    targetQuery: "",
    audience: "",
    notes: "",
  });
  const [manual, setManual] = useState({
    title: "",
    targetQuery: "",
    url: "",
    summary: "",
  });
  const [editPlan, setEditPlan] = useState({
    topic: "",
    angle: "",
    format: "",
    targetQuery: "",
    audience: "",
    notes: "",
  });
  const planButtonRefs = useRef<Record<string, HTMLButtonElement | null>>({});

  const load = useCallback(async () => {
    setRefreshing(true);
    try {
      const [planRes, inventoryRes] = await Promise.all([
        api.contentPlanItems(),
        api.contentInventory(),
      ]);
      setPlans(planRes.items);
      setInventory(inventoryRes.items);
      setError(null);
      setSelectedId((current) => current ?? planRes.items[0]?.item.plan_item_id ?? null);
      setShowCreate((current) => current || planRes.items.length === 0);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    } finally {
      setRefreshing(false);
      setLoaded(true);
    }
  }, [onUnauthorized]);

  useEffect(() => {
    void load();
  }, [load]);

  const selected = useMemo(
    () =>
      plans.find((entry) => entry.item.plan_item_id === selectedId) ??
      plans[0] ??
      null,
    [plans, selectedId],
  );

  const movePlanFocus = (
    key: "ArrowDown" | "ArrowUp" | "j" | "k" | "Home" | "End",
  ) => {
    const nextId = nextContentPlanId(plans, selected?.item.plan_item_id ?? null, key);
    if (!nextId) return;
    setSelectedId(nextId);
    window.requestAnimationFrame(() => planButtonRefs.current[nextId]?.focus());
  };

  useEffect(() => {
    if (!selected) return;
    setEditPlan({
      topic: selected.item.topic,
      angle: selected.item.angle ?? "",
      format: selected.item.format ?? "",
      targetQuery: selected.item.target_query ?? "",
      audience: selected.item.audience ?? "",
      notes: selected.item.notes ?? "",
    });
  }, [selected?.item.plan_item_id, selected?.revision]);

  const createPlan = async () => {
    if (!newPlan.topic.trim()) return;
    setBusy("create");
    setNotice(null);
    try {
      await api.createContentPlanItem({
        topic: newPlan.topic,
        angle: valueOrNull(newPlan.angle),
        format: valueOrNull(newPlan.format),
        target_query: valueOrNull(newPlan.targetQuery),
        audience: valueOrNull(newPlan.audience),
        notes: valueOrNull(newPlan.notes),
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNewPlan({ topic: "", angle: "", format: "", targetQuery: "", audience: "", notes: "" });
      setShowCreate(false);
      setNotice({ text: "Plan added.", kind: "success" });
      await load();
    } catch (err) {
      handleActionError(err, onUnauthorized, setNotice, load);
    } finally {
      setBusy(null);
    }
  };

  const updateSelected = async () => {
    if (!selected || !editPlan.topic.trim()) return;
    setBusy("update");
    setNotice(null);
    try {
      await api.updateContentPlanItem(selected.item.plan_item_id, {
        topic: editPlan.topic,
        angle: valueOrNull(editPlan.angle),
        format: valueOrNull(editPlan.format),
        target_query: valueOrNull(editPlan.targetQuery),
        audience: valueOrNull(editPlan.audience),
        notes: valueOrNull(editPlan.notes),
        expected_revision: selected.revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNotice({ text: "Plan updated.", kind: "success" });
      await load();
    } catch (err) {
      handleActionError(err, onUnauthorized, setNotice, load);
    } finally {
      setBusy(null);
    }
  };

  const checkSelected = async () => {
    if (!selected) return;
    setBusy("check");
    setNotice(null);
    try {
      await api.checkContentPlanItem(selected.item.plan_item_id, {
        expected_revision: selected.revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNotice({ text: "Overlap check refreshed.", kind: "success" });
      await load();
    } catch (err) {
      handleActionError(err, onUnauthorized, setNotice, load);
    } finally {
      setBusy(null);
    }
  };

  const refreshInventory = async () => {
    setBusy("refresh-inventory");
    setNotice(null);
    try {
      await api.refreshContentInventory({
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNotice({ text: "Published list refreshed.", kind: "success" });
      await load();
    } catch (err) {
      handleActionError(err, onUnauthorized, setNotice, load);
    } finally {
      setBusy(null);
    }
  };

  const addManual = async () => {
    if (!manual.title.trim()) return;
    setBusy("manual");
    setNotice(null);
    try {
      await api.addContentInventory({
        title: manual.title,
        target_query: valueOrNull(manual.targetQuery),
        url: valueOrNull(manual.url),
        summary: valueOrNull(manual.summary),
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setManual({ title: "", targetQuery: "", url: "", summary: "" });
      setNotice({ text: "Published item added.", kind: "success" });
      await load();
    } catch (err) {
      handleActionError(err, onUnauthorized, setNotice, load);
    } finally {
      setBusy(null);
    }
  };

  const archiveInventory = async (entry: ContentInventoryItemWithRevision) => {
    setBusy(entry.item.inventory_id);
    setNotice(null);
    try {
      await api.archiveContentInventory(entry.item.inventory_id, {
        expected_revision: entry.revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNotice({ text: "Item archived.", kind: "success" });
      await load();
    } catch (err) {
      handleActionError(err, onUnauthorized, setNotice, load);
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div>
          <h1 className="text-lg font-semibold text-zinc-100">Content campaign studio</h1>
          <p className="text-sm text-zinc-400">
            Plan keywords, research and edit each campaign, then approve blog-first publishing.
          </p>
        </div>
        <div className="flex items-center gap-2">
          {helpTopicId ? (
            <SectionHelpButton topicId={helpTopicId} onOpenHelp={onOpenHelpTopic} />
          ) : null}
          <Button variant="secondary" busy={refreshing} onClick={() => void load()}>
            {refreshing ? "Refreshing…" : "Refresh"}
          </Button>
        </div>
      </div>

      {notice ? (
        <div
          role={notice.kind === "success" ? "status" : "alert"}
          className={`rounded-md border px-3 py-2 text-sm ${noticeClass(notice.kind)}`}
        >
          {notice.text}
        </div>
      ) : null}
      {error ? (
        <div role="alert" className="rounded-md border border-red-900/60 bg-red-950/30 px-3 py-2 text-sm text-red-200">
          {error}
        </div>
      ) : null}

      <div className="inline-flex w-fit rounded-md border border-zinc-800 bg-zinc-950 p-1">
        <button aria-pressed={mode === "plans"} className={modeButton(mode === "plans")} onClick={() => setMode("plans")}>
          Plans
        </button>
        <button aria-pressed={mode === "inventory"} className={modeButton(mode === "inventory")} onClick={() => setMode("inventory")}>
          Published list
        </button>
      </div>

      {mode === "plans" ? (
        <div className="grid min-h-0 flex-1 gap-4 xl:grid-cols-[minmax(22rem,0.9fr)_minmax(28rem,1.4fr)]">
          <section className="min-h-0 max-h-[38rem] overflow-hidden rounded-lg border border-zinc-800 bg-zinc-950 xl:max-h-none">
            <div className="flex items-center justify-between gap-3 border-b border-zinc-800 px-3 py-2">
              <div>
                <h2 className="text-sm font-semibold text-zinc-200">Topics</h2>
                <p className="text-xs text-zinc-400">Use j/k or arrow keys to move through the list.</p>
              </div>
              <Button
                variant={showCreate ? "ghost" : "secondary"}
                size="sm"
                onClick={() => setShowCreate((current) => !current)}
                aria-expanded={showCreate}
              >
                {showCreate ? "Close form" : "New topic"}
              </Button>
            </div>
            {showCreate ? (
              <div className="grid gap-2 border-b border-zinc-800 p-3">
                <label className="text-xs font-medium text-zinc-400">
                  Topic <span className="text-red-300">required</span>
                  <input
                    className={`mt-1 ${FORM_CONTROL_CLS}`}
                    value={newPlan.topic}
                    onChange={(e) => setNewPlan((prev) => ({ ...prev, topic: e.target.value }))}
                  />
                </label>
                <div className="grid gap-2 sm:grid-cols-2">
                  <label className="text-xs font-medium text-zinc-400">
                    Target search
                    <input
                      className={`mt-1 ${FORM_CONTROL_CLS}`}
                      value={newPlan.targetQuery}
                      onChange={(e) =>
                        setNewPlan((prev) => ({ ...prev, targetQuery: e.target.value }))
                      }
                    />
                  </label>
                  <label className="text-xs font-medium text-zinc-400">
                    Format
                    <input
                      className={`mt-1 ${FORM_CONTROL_CLS}`}
                      value={newPlan.format}
                      onChange={(e) => setNewPlan((prev) => ({ ...prev, format: e.target.value }))}
                    />
                  </label>
                </div>
                <label className="text-xs font-medium text-zinc-400">
                  Angle
                  <input
                    className={`mt-1 ${FORM_CONTROL_CLS}`}
                    value={newPlan.angle}
                    onChange={(e) => setNewPlan((prev) => ({ ...prev, angle: e.target.value }))}
                  />
                </label>
                <label className="text-xs font-medium text-zinc-400">
                  Audience
                  <input
                    className={`mt-1 ${FORM_CONTROL_CLS}`}
                    value={newPlan.audience}
                    onChange={(e) =>
                      setNewPlan((prev) => ({ ...prev, audience: e.target.value }))
                    }
                  />
                </label>
                <label className="text-xs font-medium text-zinc-400">
                  Notes
                  <textarea
                    className={`mt-1 min-h-20 ${FORM_CONTROL_CLS}`}
                    value={newPlan.notes}
                    onChange={(e) => setNewPlan((prev) => ({ ...prev, notes: e.target.value }))}
                  />
                </label>
                <Button
                  variant="primary"
                  busy={busy === "create"}
                  onClick={() => void createPlan()}
                  disabled={!newPlan.topic.trim()}
                >
                  {busy === "create" ? "Adding…" : "Add topic"}
                </Button>
              </div>
            ) : null}
            <div className="max-h-[26rem] overflow-y-auto xl:h-full xl:max-h-none" aria-label="Content topics">
              {!loaded ? (
                <SkeletonList rows={5} />
              ) : error && plans.length === 0 ? (
                <div className="p-4">
                  <EmptyState
                    title="Topics could not be loaded"
                    action={
                      <Button variant="secondary" busy={refreshing} onClick={() => void load()}>
                        {refreshing ? "Retrying…" : "Retry"}
                      </Button>
                    }
                  >
                    Check the connection and try again. No local edits were changed.
                  </EmptyState>
                </div>
              ) : plans.length === 0 ? (
                <div className="p-4">
                  <EmptyState title="No topics yet">Complete the form above to start a campaign.</EmptyState>
                </div>
              ) : (
                plans.map((entry) => {
                  const active = selected?.item.plan_item_id === entry.item.plan_item_id;
                  const warningCount = entry.item.collision_summary?.matches.length ?? 0;
                  return (
                    <button
                      key={entry.item.plan_item_id}
                      ref={(node) => {
                        planButtonRefs.current[entry.item.plan_item_id] = node;
                      }}
                      aria-current={active ? "true" : undefined}
                      className={`w-full border-b border-l-2 border-zinc-900 px-4 py-3 text-left transition focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-sky-500/70 ${
                        active
                          ? "border-l-sky-500 bg-zinc-900"
                          : "border-l-transparent hover:bg-zinc-900/60"
                      }`}
                      onClick={() => setSelectedId(entry.item.plan_item_id)}
                      onKeyDown={(event) => {
                        if (!["ArrowDown", "ArrowUp", "j", "k", "Home", "End"].includes(event.key)) return;
                        event.preventDefault();
                        movePlanFocus(event.key as "ArrowDown" | "ArrowUp" | "j" | "k" | "Home" | "End");
                      }}
                    >
                      <div className="flex items-start justify-between gap-3">
                        <div className="min-w-0">
                          <div className="line-clamp-2 text-sm font-medium text-zinc-100">
                            {entry.item.topic}
                          </div>
                          <div className="mt-1 truncate text-xs text-zinc-400">
                            {entry.item.target_query ?? "No target search"}
                          </div>
                        </div>
                        <StatusBadge tone={planTone(entry.item.status)}>
                          {STATUS_LABEL[entry.item.status]}
                        </StatusBadge>
                      </div>
                      <div className="mt-2 flex flex-wrap gap-2 text-xs text-zinc-400">
                        <span>{draftLabel(entry)}</span>
                        {warningCount > 0 ? (
                          <span className="text-amber-300">{warningCount} overlap warning{warningCount === 1 ? "" : "s"}</span>
                        ) : (
                          <span>No overlap warnings</span>
                        )}
                      </div>
                    </button>
                  );
                })
              )}
            </div>
          </section>

          <section className="min-h-0 min-w-0 overflow-visible rounded-lg border border-zinc-800 bg-zinc-950 p-4 xl:overflow-y-auto">
            {selected ? (
              <PlanDetail
                entry={selected}
                busy={busy}
                editPlan={editPlan}
                setEditPlan={setEditPlan}
                onUpdate={updateSelected}
                onCheck={checkSelected}
                onOpenQueue={onOpenQueue}
                onUnauthorized={onUnauthorized}
                onPlanChanged={load}
              />
            ) : error ? (
              <EmptyState title="Campaign unavailable">Reload the topic list to continue.</EmptyState>
            ) : (
              <EmptyState title="Select a plan">Choose a topic to see its details and next actions.</EmptyState>
            )}
          </section>
        </div>
      ) : (
        <InventoryPanel
          inventory={inventory}
          manual={manual}
          setManual={setManual}
          busy={busy}
          onAdd={addManual}
          onArchive={archiveInventory}
          onRefresh={refreshInventory}
        />
      )}
    </div>
  );
}

function PlanDetail({
  entry,
  busy,
  editPlan,
  setEditPlan,
  onUpdate,
  onCheck,
  onOpenQueue,
  onUnauthorized,
  onPlanChanged,
}: {
  entry: ContentPlanItemWithRevision;
  busy: string | null;
  editPlan: {
    topic: string;
    angle: string;
    format: string;
    targetQuery: string;
    audience: string;
    notes: string;
  };
  setEditPlan: (value: {
    topic: string;
    angle: string;
    format: string;
    targetQuery: string;
    audience: string;
    notes: string;
  }) => void;
  onUpdate: () => Promise<void>;
  onCheck: () => Promise<void>;
  onOpenQueue: (itemId: string) => void;
  onUnauthorized: () => void;
  onPlanChanged: () => Promise<void>;
}) {
  const item = entry.item;
  const matches = item.collision_summary?.matches ?? [];
  return (
    <div className="grid gap-5">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <div className="mb-2 flex flex-wrap gap-2">
            <StatusBadge tone={planTone(item.status)}>{STATUS_LABEL[item.status]}</StatusBadge>
            <StatusBadge tone="neutral">{draftLabel(entry)}</StatusBadge>
          </div>
          <h2 className="text-lg font-semibold text-zinc-100">{item.topic}</h2>
          <p className="mt-1 text-sm text-zinc-400">{item.angle ?? "No angle set"}</p>
        </div>
        <div className="flex flex-wrap gap-2">
          <Button variant="secondary" busy={busy === "check"} onClick={() => void onCheck()}>
            {busy === "check" ? "Checking…" : "Check overlap"}
          </Button>
          <Button
            variant="secondary"
            busy={busy === "update"}
            onClick={() => void onUpdate()}
            disabled={item.status !== "planned" || !editPlan.topic.trim()}
          >
            {busy === "update" ? "Saving…" : "Save topic"}
          </Button>
          {item.work_item_id ? (
            <Button variant="secondary" onClick={() => onOpenQueue(item.work_item_id!)}>
              Open in queue
            </Button>
          ) : null}
        </div>
      </div>

      <div className="grid gap-3 md:grid-cols-2">
        <EditField
          label="Topic"
          value={editPlan.topic}
          disabled={item.status !== "planned"}
          onChange={(value) => setEditPlan({ ...editPlan, topic: value })}
        />
        <EditField
          label="Target search"
          value={editPlan.targetQuery}
          disabled={item.status !== "planned"}
          onChange={(value) => setEditPlan({ ...editPlan, targetQuery: value })}
        />
        <EditField
          label="Angle"
          value={editPlan.angle}
          disabled={item.status !== "planned"}
          onChange={(value) => setEditPlan({ ...editPlan, angle: value })}
        />
        <EditField
          label="Format"
          value={editPlan.format}
          disabled={item.status !== "planned"}
          onChange={(value) => setEditPlan({ ...editPlan, format: value })}
        />
        <EditField
          label="Audience"
          value={editPlan.audience}
          disabled={item.status !== "planned"}
          onChange={(value) => setEditPlan({ ...editPlan, audience: value })}
        />
        <ReadField label="Published URL" value={item.published_url} />
      </div>
      <EditField
        label="Notes"
        value={editPlan.notes}
        disabled={item.status !== "planned"}
        onChange={(value) => setEditPlan({ ...editPlan, notes: value })}
        multiline
      />

      <ContentCampaignWorkspace
        planItemId={item.plan_item_id}
        onUnauthorized={onUnauthorized}
        onPlanChanged={onPlanChanged}
      />

      <div className="min-w-0">
        <h3 className="mb-2 text-sm font-semibold text-zinc-200">Overlap warnings</h3>
        {matches.length === 0 ? (
          <div className="rounded-lg border border-zinc-800 bg-zinc-900/40 p-3 text-sm text-zinc-400">
            No overlaps found.
          </div>
        ) : (
          <div className="grid gap-2">
            {matches.map((match) => (
              <div
                key={`${match.inventory_id}-${match.reason}`}
                className="rounded-lg border border-zinc-800 bg-zinc-900/40 p-3"
              >
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <div className="text-sm font-medium text-zinc-100">{match.title}</div>
                  <StatusBadge tone={match.reason === "similar" ? "warning" : "critical"}>
                    {reasonLabel(match)}
                  </StatusBadge>
                </div>
                <div className="mt-1 text-xs text-zinc-400">
                  {sourceLabel(match.source_kind)}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function InventoryPanel({
  inventory,
  manual,
  setManual,
  busy,
  onAdd,
  onArchive,
  onRefresh,
}: {
  inventory: ContentInventoryItemWithRevision[];
  manual: { title: string; targetQuery: string; url: string; summary: string };
  setManual: (value: { title: string; targetQuery: string; url: string; summary: string }) => void;
  busy: string | null;
  onAdd: () => Promise<void>;
  onArchive: (entry: ContentInventoryItemWithRevision) => Promise<void>;
  onRefresh: () => Promise<void>;
}) {
  return (
    <div className="grid min-h-0 flex-1 gap-4 xl:grid-cols-[minmax(24rem,0.9fr)_minmax(28rem,1.3fr)]">
      <section className="rounded-lg border border-zinc-800 bg-zinc-950 p-4">
        <div className="mb-4 flex flex-col items-start justify-between gap-3 sm:flex-row sm:items-center">
          <h2 className="text-base font-semibold text-zinc-100">Published list</h2>
          <Button
            variant="secondary"
            busy={busy === "refresh-inventory"}
            onClick={() => void onRefresh()}
          >
            {busy === "refresh-inventory" ? "Refreshing…" : "Refresh published list"}
          </Button>
        </div>
        <div className="grid gap-2">
          <label className="text-xs font-medium text-zinc-400">
            Title <span className="text-red-300">required</span>
            <input
              className={`mt-1 ${FORM_CONTROL_CLS}`}
              value={manual.title}
              onChange={(e) => setManual({ ...manual, title: e.target.value })}
            />
          </label>
          <label className="text-xs font-medium text-zinc-400">
            Target search
            <input
              className={`mt-1 ${FORM_CONTROL_CLS}`}
              value={manual.targetQuery}
              onChange={(e) => setManual({ ...manual, targetQuery: e.target.value })}
            />
          </label>
          <label className="text-xs font-medium text-zinc-400">
            Published URL
            <input
              type="url"
              className={`mt-1 ${FORM_CONTROL_CLS}`}
              value={manual.url}
              onChange={(e) => setManual({ ...manual, url: e.target.value })}
            />
          </label>
          <label className="text-xs font-medium text-zinc-400">
            Summary
            <textarea
              className={`mt-1 min-h-24 ${FORM_CONTROL_CLS}`}
              value={manual.summary}
              onChange={(e) => setManual({ ...manual, summary: e.target.value })}
            />
          </label>
          <Button
            variant="primary"
            busy={busy === "manual"}
            onClick={() => void onAdd()}
            disabled={!manual.title.trim()}
          >
            {busy === "manual" ? "Adding…" : "Add published item"}
          </Button>
        </div>
      </section>

      <section className="min-h-0 overflow-y-auto rounded-lg border border-zinc-800 bg-zinc-950">
        {inventory.length === 0 ? (
          <div className="p-4">
            <EmptyState title="No published items">Refresh or add an item manually.</EmptyState>
          </div>
        ) : (
          inventory.map((entry) => (
            <div key={entry.item.inventory_id} className="border-b border-zinc-900 p-4">
              <div className="flex flex-wrap items-start justify-between gap-3">
                <div className="min-w-0">
                  <div className="truncate text-sm font-medium text-zinc-100">
                    {entry.item.title}
                  </div>
                  <div className="mt-1 truncate text-xs text-zinc-400">
                    {entry.item.url ?? entry.item.target_query ?? "No URL"}
                  </div>
                </div>
                <div className="flex items-center gap-2">
                  <StatusBadge tone={entry.item.status === "archived" ? "warning" : "ok"}>
                    {entry.item.status === "archived" ? "Archived" : entry.item.status === "pipeline" ? "Planned" : "Published"}
                  </StatusBadge>
                  <StatusBadge tone="neutral">{sourceLabel(entry.item.source_kind)}</StatusBadge>
                </div>
              </div>
              {entry.item.summary ? (
                <p className="mt-2 line-clamp-2 text-sm text-zinc-400">{entry.item.summary}</p>
              ) : null}
              {entry.item.status !== "archived" ? (
                <div className="mt-3">
                  <Button
                    variant="secondary"
                    size="sm"
                    onClick={() => void onArchive(entry)}
                    disabled={busy === entry.item.inventory_id}
                  >
                    Archive
                  </Button>
                </div>
              ) : null}
            </div>
          ))
        )}
      </section>
    </div>
  );
}

function EditField({
  label,
  value,
  disabled,
  onChange,
  multiline,
}: {
  label: string;
  value: string;
  disabled: boolean;
  onChange: (value: string) => void;
  multiline?: boolean;
}) {
  const className =
    "w-full rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 outline-none focus-visible:border-sky-600 focus-visible:ring-2 focus-visible:ring-sky-500/30 disabled:cursor-not-allowed disabled:bg-zinc-900 disabled:text-zinc-400";
  return (
    <label className="rounded-lg border border-zinc-800 bg-zinc-900/40 p-3">
      <span className="mb-1 block text-xs font-medium text-zinc-400">{label}</span>
      {multiline ? (
        <textarea
          className={`${className} min-h-28`}
          value={value}
          disabled={disabled}
          onChange={(event) => onChange(event.target.value)}
        />
      ) : (
        <input
          className={className}
          value={value}
          disabled={disabled}
          onChange={(event) => onChange(event.target.value)}
        />
      )}
    </label>
  );
}

function ReadField({
  label,
  value,
}: {
  label: string;
  value: string | null | undefined;
}) {
  return (
    <div className="min-w-0 rounded-lg border border-zinc-800 bg-zinc-900/40 p-3">
      <div className="mb-1 text-xs font-medium text-zinc-400">{label}</div>
      <div className="break-words whitespace-pre-wrap text-sm text-zinc-200">{value?.trim() || "Not set"}</div>
    </div>
  );
}

function valueOrNull(value: string): string | null {
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}

function modeButton(active: boolean): string {
  return `rounded px-3 py-1.5 text-sm font-medium focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70 ${
    active ? "bg-zinc-800 text-zinc-100" : "text-zinc-400 hover:text-zinc-200"
  }`;
}

function noticeClass(kind: NonNullable<Notice>["kind"]): string {
  if (kind === "success") return "border-emerald-900/60 bg-emerald-950/30 text-emerald-200";
  if (kind === "conflict") return "border-amber-900/60 bg-amber-950/30 text-amber-200";
  return "border-red-900/60 bg-red-950/30 text-red-200";
}

function handleActionError(
  err: unknown,
  onUnauthorized: () => void,
  setNotice: (notice: Notice) => void,
  load: () => Promise<void>,
) {
  if (isUnauthorized(err)) {
    onUnauthorized();
    return;
  }
  if (isRevisionConflict(err)) {
    setNotice({ text: "Changed elsewhere — reload and try again.", kind: "conflict" });
    void load();
    return;
  }
  setNotice({ text: errorMessage(err), kind: "error" });
}
