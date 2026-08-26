import { useCallback, useEffect, useRef, useState } from "react";
import { useAppCommand } from "../lib/commands";
import { usePolling } from "../lib/usePolling";
import type { TaskDueLane } from "../types/generated/TaskDueLane";
import type { TaskEscalation } from "../types/generated/TaskEscalation";
import type { TaskStatus } from "../types/generated/TaskStatus";
import type { TaskWithRevision } from "../types/generated/TaskWithRevision";
import {
  api,
  errorMessage,
  isRevisionConflict,
  isUnauthorized,
} from "../lib/api";
import SectionHelpButton from "../components/SectionHelpButton";
import { Button, EmptyState, StatusBadge } from "../components/ui";
import { canDraftFollowUpReply, threadStateChip } from "../lib/followUp";

type Notice = { text: string; kind: "error" | "conflict" | "success" } | null;

const POLL_INTERVAL_MS = 30_000;

type Filter = TaskStatus | "all";

const FILTERS: { id: Filter; label: string }[] = [
  { id: "open", label: "Open" },
  { id: "done", label: "Done" },
  { id: "all", label: "All" },
];

/** Browser-local date as YYYY-MM-DD — the server classifies lanes against
 * the operator's day, not the server's timezone. */
export function localToday(): string {
  const now = new Date();
  const y = now.getFullYear();
  const m = String(now.getMonth() + 1).padStart(2, "0");
  const d = String(now.getDate()).padStart(2, "0");
  return `${y}-${m}-${d}`;
}

type LaneTone = "critical" | "warning" | "neutral";

/** Lane sections for the open list, in watchdog order. */
const LANES: { id: TaskDueLane; label: string; tone: LaneTone }[] = [
  { id: "overdue", label: "Overdue", tone: "critical" },
  { id: "due_today", label: "Due today", tone: "warning" },
  { id: "upcoming", label: "Upcoming", tone: "neutral" },
  { id: "no_due_date", label: "No due date", tone: "neutral" },
];

const laneToneTextCls: Record<LaneTone, string> = {
  critical: "text-red-400",
  warning: "text-amber-300",
  neutral: "text-zinc-400",
};

function EscalationBadge({
  escalation,
}: {
  escalation: TaskEscalation | null | undefined;
}) {
  if (!escalation || escalation.lane === "no_due_date") return null;
  switch (escalation.lane) {
    case "overdue": {
      const days = escalation.days_overdue;
      const label =
        escalation.level === "critical"
          ? `critical · ${days}d overdue`
          : escalation.level === "escalated"
            ? `escalated · ${days}d overdue`
            : `overdue ${days}d`;
      return (
        <StatusBadge
          tone="critical"
          title={escalation.reason ?? undefined}
        >
          {label}
        </StatusBadge>
      );
    }
    case "due_today":
      return (
        <StatusBadge tone="warning" title={escalation.reason ?? undefined}>
          due today
        </StatusBadge>
      );
    default: {
      const days = escalation.days_until_due;
      const label = days === 1 ? "due tomorrow" : `due in ${days}d`;
      const tone = days <= 1 ? "warning" : "neutral";
      return (
        <StatusBadge tone={tone} title={escalation.reason ?? undefined}>
          {label}
        </StatusBadge>
      );
    }
  }
}

export default function Tasks({
  onUnauthorized,
  helpTopicId,
  onOpenHelpTopic,
  agentLaunchEnabled,
  focusTaskId,
  onFocusTaskConsumed,
}: {
  onUnauthorized: () => void;
  helpTopicId?: string;
  onOpenHelpTopic: (topicId: string) => void;
  agentLaunchEnabled: boolean;
  focusTaskId?: string | null;
  onFocusTaskConsumed?: () => void;
}) {
  const [tasks, setTasks] = useState<TaskWithRevision[]>([]);
  const [filter, setFilter] = useState<Filter>("open");
  const [loaded, setLoaded] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<Notice>(null);
  const [busyTaskId, setBusyTaskId] = useState<string | null>(null);
  const [launchingTaskId, setLaunchingTaskId] = useState<string | null>(null);
  const [focusedTaskId, setFocusedTaskId] = useState<string | null>(null);
  const [expandedTaskIds, setExpandedTaskIds] = useState<Set<string>>(
    () => new Set(),
  );
  const rowRefs = useRef(new Map<string, HTMLDivElement>());
  const inFlightTaskIds = useRef(new Set<string>());
  // Follow-up workflow actions (Re-check / Draft follow-up reply) keyed by
  // follow_up_id, tracked separately from the complete/reopen task action.
  const [followUpBusy, setFollowUpBusy] = useState<{
    id: string;
    kind: "check" | "draft";
  } | null>(null);
  const followUpInFlight = useRef(new Set<string>());

  const load = useCallback(async () => {
    setRefreshing(true);
    try {
      const res = await api.tasks(undefined, localToday());
      setTasks(res.tasks);
      setError(null);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    } finally {
      setRefreshing(false);
      setLoaded(true);
    }
  }, [onUnauthorized]);

  useAppCommand("refresh", () => void load());

  usePolling(load, { intervalMs: POLL_INTERVAL_MS });

  const runAction = async (
    entry: TaskWithRevision,
    action: "complete" | "reopen",
  ) => {
    const taskId = entry.task.task_id;
    if (inFlightTaskIds.current.has(taskId)) return;
    inFlightTaskIds.current.add(taskId);
    setBusyTaskId(taskId);
    setNotice(null);

    // Snapshot for potential revert
    const snapshot = entry;

    // Optimistic update: patch status immediately
    const optimisticStatus = action === "complete" ? "done" : "open";
    setTasks((prev) =>
      prev.map((e) =>
        e.task.task_id === entry.task.task_id
          ? { ...e, task: { ...e.task, status: optimisticStatus } }
          : e,
      ),
    );

    try {
      const res = await api.taskAction(taskId, {
        action,
        expected_revision: entry.revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      // Patch the revision from the server response
      const newRevision = res.revision ?? snapshot.revision + 1;
      setTasks((prev) =>
        prev.map((e) =>
          e.task.task_id === entry.task.task_id
            ? { ...e, revision: newRevision }
            : e,
        ),
      );
      // Silent background reconcile for escalation lane re-classification
      void load();
    } catch (err) {
      if (isUnauthorized(err)) {
        onUnauthorized();
      } else if (isRevisionConflict(err)) {
        setNotice({ text: "Task changed elsewhere — reloaded the list.", kind: "conflict" });
        await load();
      } else {
        // Revert optimistic update
        setTasks((prev) =>
          prev.map((e) =>
            e.task.task_id === snapshot.task.task_id ? snapshot : e,
          ),
        );
        setNotice({ text: `Action failed: ${errorMessage(err)}`, kind: "error" });
      }
    } finally {
      inFlightTaskIds.current.delete(taskId);
      setBusyTaskId(null);
    }
  };

  // Quiet manual reconciliation: re-read the Gmail thread state. Updates the
  // chip in place and reloads (a reply may have auto-resolved the task → Done).
  const recheckFollowUp = async (entry: TaskWithRevision) => {
    const followUpId = entry.follow_up?.follow_up_id;
    if (!followUpId || followUpInFlight.current.has(followUpId)) return;
    followUpInFlight.current.add(followUpId);
    setFollowUpBusy({ id: followUpId, kind: "check" });
    setNotice(null);
    try {
      const res = await api.emailFollowUpCheck(followUpId, {
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      // Patch the summary in place for instant feedback…
      setTasks((prev) =>
        prev.map((e) =>
          e.follow_up?.follow_up_id === followUpId
            ? { ...e, follow_up: res.follow_up }
            : e,
        ),
      );
      // …then reconcile status/lane (auto-resolution moves the task to Done).
      void load();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else
        setNotice({
          text: `Couldn't re-check this thread: ${errorMessage(err)}`,
          kind: "error",
        });
    } finally {
      followUpInFlight.current.delete(followUpId);
      setFollowUpBusy(null);
    }
  };

  const toggleExpanded = (taskId: string) => {
    setExpandedTaskIds((prev) => {
      const next = new Set(prev);
      if (next.has(taskId)) next.delete(taskId);
      else next.add(taskId);
      return next;
    });
  };

  const launchAgentForTask = async (entry: TaskWithRevision) => {
    const itemId = entry.task.source_item_id;
    if (!itemId) return;
    setLaunchingTaskId(entry.task.task_id);
    setNotice(null);
    try {
      const res = await api.launchAgent(itemId, {
        context: [
          `Task: ${entry.task.title}`,
          entry.task.due_date ? `Due date: ${entry.task.due_date}` : null,
          entry.task.context ? `Task context: ${entry.task.context}` : null,
        ]
          .filter(Boolean)
          .join("\n"),
        work_dir: null,
        attachment_ids: [],
        idempotency_key: crypto.randomUUID(),
      });
      setNotice({
        text: `Agent session started: ${res.session_id}${
          res.thread_id ? ` (thread ${res.thread_id})` : ""
        }`,
        kind: "success",
      });
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else
        setNotice({
          text: `Agent launch failed: ${errorMessage(err)}`,
          kind: "error",
        });
    } finally {
      setLaunchingTaskId(null);
    }
  };

  // Explicit operator action at due-time: open a normal email_draft_reply work
  // item for the follow-up. Still Gmail DRAFT only; never auto-send.
  const draftFollowUpReply = async (entry: TaskWithRevision) => {
    const followUpId = entry.follow_up?.follow_up_id;
    if (!followUpId || followUpInFlight.current.has(followUpId)) return;
    followUpInFlight.current.add(followUpId);
    setFollowUpBusy({ id: followUpId, kind: "draft" });
    setNotice(null);
    try {
      await api.emailFollowUpDraft(followUpId, {
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNotice({
        text: "Follow-up reply started — review and approve it from the Queue (we never send on your behalf).",
        kind: "conflict",
      });
      void load();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else
        setNotice({
          text: `Couldn't start a follow-up reply: ${errorMessage(err)}`,
          kind: "error",
        });
    } finally {
      followUpInFlight.current.delete(followUpId);
      setFollowUpBusy(null);
    }
  };

  const counts = new Map<Filter, number>([["all", tasks.length]]);
  for (const e of tasks) {
    counts.set(e.task.status, (counts.get(e.task.status) ?? 0) + 1);
  }
  const visible =
    filter === "all" ? tasks : tasks.filter((e) => e.task.status === filter);

  const laneGroups =
    filter === "open"
      ? LANES.map((lane) => ({
          lane,
          entries: visible.filter(
            (e) => (e.escalation?.lane ?? "no_due_date") === lane.id,
          ),
        })).filter((group) => group.entries.length > 0)
      : null;

  useEffect(() => {
    if (focusedTaskId) {
      rowRefs.current.get(focusedTaskId)?.scrollIntoView({ block: "nearest" });
    }
  }, [focusedTaskId]);

  useEffect(() => {
    if (!focusTaskId || !loaded) return;
    const idx = visible.findIndex((entry) => entry.task.task_id === focusTaskId);
    if (idx === -1) {
      onFocusTaskConsumed?.();
      return;
    }
    setFocusedTaskId(focusTaskId);
    setExpandedTaskIds((prev) => {
      const next = new Set(prev);
      next.add(focusTaskId);
      return next;
    });
    requestAnimationFrame(() => {
      rowRefs.current.get(focusTaskId)?.scrollIntoView({ block: "center" });
    });
    onFocusTaskConsumed?.();
  }, [focusTaskId, loaded, onFocusTaskConsumed, visible]);

  const renderRow = (entry: TaskWithRevision) => {
    const task = entry.task;
    const busy = busyTaskId === task.task_id;
    const launching = launchingTaskId === task.task_id;
    const expanded = expandedTaskIds.has(task.task_id);
    const done = task.status === "done";

    // Outbound follow-up decoration (issue #185), present only on follow-up
    // tasks whose Gmail thread we're tracking.
    const followUp = entry.follow_up ?? null;
    const followUpId = followUp?.follow_up_id ?? null;
    const chip = threadStateChip(followUp?.thread_state);
    const threadState = followUp?.thread_state ?? null;
    const fuBusy = followUpBusy?.id === followUpId;
    const checking = fuBusy && followUpBusy?.kind === "check";
    const drafting = fuBusy && followUpBusy?.kind === "draft";
    // Re-check is offered while the thread is still open (not yet auto-resolved
    // and not "not applicable", which renders no chip).
    const canRecheck =
      !done && followUpId != null && chip != null && threadState !== "replied_after_send";
    const canDraftReply =
      !done &&
      followUpId != null &&
      canDraftFollowUpReply({ threadState, dueLane: entry.escalation?.lane });

    return (
      <div
        key={task.task_id}
        ref={(el) => {
          if (el) rowRefs.current.set(task.task_id, el);
          else rowRefs.current.delete(task.task_id);
        }}
        className={`flex flex-col gap-2 px-3 py-2.5 hover:bg-zinc-900/60 ${
          focusedTaskId === task.task_id ? "bg-zinc-900/60" : ""
        }`}
      >
        <div className="flex items-center gap-3">
          <div className="min-w-0 flex-1">
            <div className="flex items-baseline gap-2">
              <span
                className={`min-w-0 text-sm font-semibold ${
                  expanded ? "whitespace-normal" : "truncate"
                } ${done ? "text-zinc-500 line-through" : "text-zinc-100"}`}
              >
                {task.title}
              </span>
              {!done ? <EscalationBadge escalation={entry.escalation} /> : null}
              {/* Thread-state chip shows on open AND done rows; for an
                  auto-resolved task it reads "They replied" (green) silently. */}
              {chip ? <StatusBadge tone={chip.tone}>{chip.label}</StatusBadge> : null}
            </div>
            {task.context ? (
              <div
                className={`mt-0.5 text-xs text-zinc-400 ${
                  expanded ? "whitespace-pre-wrap" : "truncate"
                }`}
              >
                {task.context}
              </div>
            ) : null}
          </div>
          <span className="shrink-0 whitespace-nowrap text-xs text-zinc-400">
            {task.due_date ?? ""}
          </span>
          <div className="flex shrink-0 items-center gap-2">
            {canRecheck ? (
              <button
                type="button"
                disabled={fuBusy}
                onClick={() => void recheckFollowUp(entry)}
                className="text-xs text-zinc-400 underline-offset-2 hover:text-zinc-200 hover:underline disabled:opacity-50"
                title="Re-read the Gmail thread to see if they've replied"
              >
                {checking ? "Re-checking…" : "Re-check"}
              </button>
            ) : null}
            {canDraftReply ? (
              <Button
                variant="primary"
                size="sm"
                busy={drafting}
                disabled={fuBusy}
                onClick={() => void draftFollowUpReply(entry)}
              >
                Draft follow-up reply
              </Button>
            ) : null}
            <Button
              variant="ghost"
              size="sm"
              onClick={() => toggleExpanded(task.task_id)}
            >
              {expanded ? "Hide" : "View"}
            </Button>
            {agentLaunchEnabled && task.source_item_id ? (
              <Button
                variant="secondary"
                size="sm"
                busy={launching}
                disabled={launching}
                onClick={() => void launchAgentForTask(entry)}
              >
                Agent
              </Button>
            ) : null}
            {done ? (
              <Button
                variant="secondary"
                size="sm"
                busy={busy}
                onClick={() => void runAction(entry, "reopen")}
              >
                Reopen
              </Button>
            ) : (
              <Button
                variant="success"
                size="sm"
                busy={busy}
                onClick={() => void runAction(entry, "complete")}
              >
                Done
              </Button>
            )}
          </div>
        </div>
        {expanded ? (
          <div className="rounded-md border border-zinc-800 bg-zinc-950/70 px-3 py-2 text-xs text-zinc-400">
            <div className="whitespace-pre-wrap text-zinc-300">{task.context || "No context."}</div>
            <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1 font-mono text-[11px] text-zinc-500">
              <span>task {task.task_id}</span>
              <span>
                source {task.source_kind}:{task.source_ref}
              </span>
              {task.source_item_id ? <span>item {task.source_item_id}</span> : null}
            </div>
          </div>
        ) : null}
      </div>
    );
  };

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-2">
          <h2 className="text-lg font-semibold text-zinc-100">Tasks</h2>
          <SectionHelpButton
            topicId={helpTopicId}
            onOpenHelp={onOpenHelpTopic}
            label="Open help for Tasks"
          />
        </div>
        <div className="flex items-center gap-3">
          <span className="text-xs text-zinc-500">polls every 30s</span>
          <Button
            variant="secondary"
            size="sm"
            busy={refreshing}
            onClick={() => void load()}
          >
            {refreshing ? "Refreshing…" : "Refresh"}
          </Button>
        </div>
      </div>

      <div className="flex items-center gap-1">
        {FILTERS.map((f) => (
          <button
            key={f.id}
            onClick={() => setFilter(f.id)}
            className={`rounded-full px-3 py-1 text-xs font-medium transition ${
              filter === f.id
                ? "bg-zinc-800 text-zinc-100 ring-1 ring-inset ring-zinc-600"
                : "text-zinc-400 hover:bg-zinc-900 hover:text-zinc-200"
            }`}
          >
            {f.label}
            <span className="ml-1.5 text-zinc-500">{counts.get(f.id) ?? 0}</span>
          </button>
        ))}
      </div>

      {error ? (
        <div className="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300">
          Failed to load tasks: {error}
        </div>
      ) : null}
      {notice ? (
        <div
          className={`rounded-md border px-3 py-2 text-sm ${
            notice.kind === "error"
              ? "border-red-900/60 bg-red-950/40 text-red-300"
              : notice.kind === "success"
                ? "border-emerald-900/60 bg-emerald-950/30 text-emerald-300"
              : "border-amber-900/60 bg-amber-950/30 text-amber-300"
          }`}
        >
          {notice.text}
        </div>
      ) : null}

      {loaded && visible.length === 0 && !error ? (
        filter === "open" ? (
          <EmptyState
            variant="celebrate"
            title="Queue clear — nothing needs you."
          >
            Tasks land here when you approve a follow-up draft on an accepted
            Queue item (the &ldquo;Drafts&rdquo; button).
          </EmptyState>
        ) : (
          <EmptyState title={`No ${filter === "all" ? "" : `${filter} `}tasks.`}>
            Change the filter to see tasks in other states.
          </EmptyState>
        )
      ) : null}

      {laneGroups
        ? laneGroups.map((group) => (
            <div key={group.lane.id} className="flex flex-col gap-1.5">
              <h3
                className={`surface-section-head surface-head-amber text-xs font-semibold uppercase tracking-wide ${laneToneTextCls[group.lane.tone]}`}
              >
                {group.lane.label}
                <span className="ml-1.5 font-normal text-zinc-500">
                  {group.entries.length}
                </span>
              </h3>
              <div className="surface-card surface-flat surface-body-amber surface-row-divide divide-y divide-zinc-800/80 rounded-lg border border-zinc-800">
                {group.entries.map(renderRow)}
              </div>
            </div>
          ))
        : visible.length > 0 && (
            <div className="surface-card surface-flat surface-body-amber surface-row-divide divide-y divide-zinc-800/80 overflow-hidden rounded-lg border border-zinc-800">
              {visible.map(renderRow)}
            </div>
          )}
    </div>
  );
}
