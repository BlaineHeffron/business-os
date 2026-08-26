import { Fragment, useCallback, useEffect, useMemo, useState } from "react";
import type { CategoryRecord } from "../types/generated/CategoryRecord";
import type { EmailTriageGmailCategory } from "../types/generated/EmailTriageGmailCategory";
import type { PacketKindRecord } from "../types/generated/PacketKindRecord";
import type { WorkQueuePolicy } from "../types/generated/WorkQueuePolicy";
import { ApiError, api, errorMessage, isUnauthorized } from "../lib/api";
import { useAppCommand } from "../lib/commands";
import {
  CATEGORY_ID_PATTERN,
  DEFAULT_CATEGORY_COLOR,
  FALLBACK_CATEGORY_ID,
  defaultWorkQueuePolicy,
  normalizeWorkQueuePolicy,
  useCategories,
} from "../lib/categories";
import { usePacketKinds } from "../lib/packetKinds";
import SectionHelpButton from "../components/SectionHelpButton";
import {
  Button,
  ConfirmDialog,
  EmptyState,
  cellCls,
  rowDivideCls,
  rowHoverCls,
  tableCls,
  tableWrapCls,
  theadCls,
} from "../components/ui";

// Sentinel stored in a policy's ai_suggestible_packet_kinds to mean "the AI may
// suggest any enabled packet kind it judges appropriate" (it chooses). Must
// match bos-contracts work_queue::AI_SUGGEST_ALL_SENTINEL on the backend.
const AI_SUGGEST_ALL = "*";
const GMAIL_TABS: { id: EmailTriageGmailCategory; label: string }[] = [
  { id: "primary", label: "Primary" },
  { id: "updates", label: "Updates" },
  { id: "social", label: "Social" },
  { id: "promotions", label: "Promotions" },
  { id: "forums", label: "Forums" },
];

interface Draft {
  category_id: string;
  display_name: string;
  description: string;
  color: string;
  sort: string;
  is_system: boolean;
  default_agent_dir: string;
  default_agent_context: string;
}

function emptyDraft(nextSort: number): Draft {
  return {
    category_id: "",
    display_name: "",
    description: "",
    color: DEFAULT_CATEGORY_COLOR,
    sort: String(nextSort),
    is_system: false,
    default_agent_dir: "",
    default_agent_context: "",
  };
}

function draftFromRecord(record: CategoryRecord): Draft {
  return {
    category_id: record.category_id,
    display_name: record.display_name,
    description: record.description,
    color: record.color,
    sort: String(record.sort),
    is_system: record.is_system,
    default_agent_dir: record.default_agent_dir ?? "",
    default_agent_context: record.default_agent_context ?? "",
  };
}

function validate(draft: Draft, creating: boolean): string[] {
  const errors: string[] = [];
  if (creating && !CATEGORY_ID_PATTERN.test(draft.category_id)) {
    errors.push(
      "Category ID must be lowercase letters, numbers, or underscores (max 64 characters).",
    );
  }
  if (draft.display_name.trim().length === 0) {
    errors.push("display name is required.");
  }
  if (!Number.isInteger(Number(draft.sort))) {
    errors.push("sort must be an integer.");
  }
  return errors;
}

function deleteErrorMessage(err: unknown): string {
  if (err instanceof ApiError) {
    switch (err.code) {
      case "email_triage_category_is_system":
        return "System category can't be deleted.";
      case "email_triage_category_in_use":
        return "In use by a rule — repoint or delete the rule first.";
      case "email_triage_category_not_found":
        return "Category not found — it may already be deleted.";
    }
  }
  return `Delete failed: ${errorMessage(err)}`;
}

// Compact packet-kind selector. Collapsed row shows "N drafts · M AI" summary;
// clicking expands an inline picker with draft and AI kind rows. Legacy kind
// entries (in policy but absent from catalog) are shown as removal-only chips,
// never duplicated alongside their catalog equivalents.
// One compact on/off cell used by both the draft and AI columns of the
// packet-kind picker.
function KindToggle({
  on,
  busy,
  activeCls,
  title,
  onClick,
}: {
  on: boolean;
  busy: boolean;
  activeCls: string;
  title: string;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      disabled={busy}
      role="switch"
      aria-checked={on}
      title={title}
      className={`w-full rounded-full border px-2 py-0.5 text-xs transition disabled:opacity-40 ${
        on
          ? activeCls
          : "border-zinc-700 text-zinc-500 hover:bg-zinc-800 hover:text-zinc-200"
      }`}
    >
      {on ? "On" : "Off"}
    </button>
  );
}

function PacketKindPicker({
  policy,
  catalog,
  onChange,
  busy,
  autoProduceEnabled,
  aiTriageEnabled,
}: {
  policy: WorkQueuePolicy;
  catalog: PacketKindRecord[];
  onChange: (policy: WorkQueuePolicy) => void;
  busy: boolean;
  autoProduceEnabled: boolean;
  aiTriageEnabled: boolean;
}) {
  const [open, setOpen] = useState(false);

  // Draft kinds in the policy that are no longer in the platform catalog.
  const legacyKinds = policy.packet_kinds.filter(
    (id) => !catalog.some((k) => k.kind_id === id),
  );
  const produceCount = policy.packet_kinds.length - legacyKinds.length;

  // AI suggestion is a single switch, not a per-kind selection: when on, the
  // AI triage pass may add any enabled packet kind it judges appropriate (it
  // chooses). Stored as the AI_SUGGEST_ALL sentinel so future kinds are
  // covered; any non-empty list reads as "on".
  const aiOn = policy.ai_suggestible_packet_kinds.length > 0;
  const canScopeAiTabs = policy.category_id === FALLBACK_CATEGORY_ID;
  const aiScope = policy.ai_suggestible_gmail_scope;
  const scopedTabs =
    aiScope === "selected" || aiScope === "default"
      ? policy.ai_suggestible_gmail_categories
      : [];

  const summary =
    produceCount === 0
      ? aiOn
        ? "AI suggestions"
        : "no drafts"
      : `${produceCount} draft${produceCount === 1 ? "" : "s"}${aiOn ? " · AI" : ""}`;

  return (
    <div>
      <button
        onClick={() => setOpen((v) => !v)}
        className="flex items-center gap-1 text-xs text-zinc-400 transition hover:text-zinc-200"
        title={open ? "Collapse output settings" : "Expand output settings"}
      >
        <span>{summary}</span>
        {legacyKinds.length > 0 && (
          <span className="rounded-full bg-amber-950/60 px-1.5 py-0.5 text-xs text-amber-400">
            {legacyKinds.length} legacy
          </span>
        )}
        <span className="text-zinc-600">{open ? "▲" : "▼"}</span>
      </button>

      {open && (
        <div className="mt-2 flex flex-col gap-2">
          {/* Produced packet kinds — what the policy deterministically
              attaches to every item in this category. */}
          <div className="min-w-64 overflow-hidden rounded-md border border-zinc-800">
            <div className="border-b border-zinc-800 bg-zinc-900/60 px-2 py-1 text-[10px] font-medium uppercase tracking-wide text-zinc-500">
              Drafts to create
            </div>
            {catalog.map((k) => {
              const produce = policy.packet_kinds.includes(k.kind_id);
              return (
                <div
                  key={k.kind_id}
                  className="flex items-center gap-2 border-t border-zinc-800/70 px-2 py-1 first:border-t-0"
                >
                  <span
                    className="flex-1 truncate text-xs text-zinc-300"
                    title={
                      k.produce_available ? k.description : `${k.description} (soon)`
                    }
                  >
                    {k.title}
                  </span>
                  <div className="w-16">
                    <KindToggle
                      on={produce}
                      busy={busy}
                      activeCls="border-sky-700 bg-sky-950/60 text-sky-300"
                      title={`Create ${k.title} for items in this category`}
                      onClick={() =>
                        onChange({
                          ...policy,
                          packet_kinds: produce
                            ? policy.packet_kinds.filter((id) => id !== k.kind_id)
                            : [...policy.packet_kinds, k.kind_id],
                        })
                      }
                    />
                  </div>
                </div>
              );
            })}
            {legacyKinds.map((id) => (
              <div
                key={`legacy-${id}`}
                className="flex items-center gap-2 border-t border-zinc-800/70 px-2 py-1"
              >
                <span className="flex flex-1 items-center gap-1 truncate font-mono text-xs text-zinc-400">
                  {id}
                  <span className="rounded-full bg-amber-950/60 px-1.5 py-0.5 text-[10px] text-amber-400">
                    legacy
                  </span>
                </span>
                <div className="w-16">
                  <button
                    onClick={() =>
                      onChange({
                        ...policy,
                        packet_kinds: policy.packet_kinds.filter((k) => k !== id),
                      })
                    }
                    disabled={busy}
                    title="No longer recognized — remove"
                    className="w-full rounded-full border border-zinc-600 px-2 py-0.5 text-xs text-zinc-300 transition hover:border-red-900/70 hover:text-red-400 disabled:opacity-40"
                  >
                    remove
                  </button>
                </div>
              </div>
            ))}
          </div>

          <div className="flex flex-wrap gap-1.5">
            {aiTriageEnabled ? (
              <>
                {/* AI suggestions — one switch; the AI picks which kinds to add. */}
                <button
                  onClick={() => {
                    if (!aiTriageEnabled) return;
                    const nextAiOn = !aiOn;
                    onChange({
                      ...policy,
                      ai_suggestible_packet_kinds: nextAiOn
                        ? [AI_SUGGEST_ALL]
                        : [],
                      ai_suggestible_gmail_scope: nextAiOn
                        ? policy.ai_suggestible_gmail_scope === "default" &&
                          policy.ai_suggestible_gmail_categories.length === 0
                          ? "all"
                          : policy.ai_suggestible_gmail_scope
                        : "default",
                      ai_suggestible_gmail_categories: nextAiOn
                        ? policy.ai_suggestible_gmail_categories
                        : [],
                    });
                  }}
                  disabled={busy}
                  role="switch"
                  aria-checked={aiOn}
                  title="When on, AI may suggest extra draft types for an email that warrants them, choosing from the outputs listed above. Off = only the fixed outputs above."
                  className={`self-start rounded-full border px-2 py-0.5 text-xs transition disabled:opacity-40 ${
                    aiOn
                      ? "border-violet-700 bg-violet-950/60 text-violet-300"
                      : "border-zinc-700 text-zinc-500 hover:bg-zinc-800 hover:text-zinc-200"
                  }`}
                >
                  ✨ AI suggestions {aiOn ? "on" : "off"}
                </button>
                {aiOn && canScopeAiTabs ? (
                  <div className="flex flex-wrap items-center gap-1.5">
                    <span className="text-xs text-zinc-500">tabs</span>
                    <button
                      type="button"
                      disabled={busy}
                      aria-pressed={aiScope === "all"}
                      title="Allow AI triage for all fallback mail"
                      onClick={() =>
                        onChange({
                          ...policy,
                          ai_suggestible_gmail_scope: "all",
                          ai_suggestible_gmail_categories: [],
                        })
                      }
                      className={`rounded-full border px-2 py-0.5 text-xs transition disabled:opacity-40 ${
                        aiScope === "all"
                          ? "border-sky-700 bg-sky-950/60 text-sky-300"
                          : "border-zinc-700 text-zinc-500 hover:bg-zinc-800 hover:text-zinc-200"
                      }`}
                    >
                      All
                    </button>
                    {GMAIL_TABS.map((tab) => {
                      const selected = scopedTabs.includes(tab.id);
                      return (
                        <button
                          key={tab.id}
                          type="button"
                          disabled={busy}
                          aria-pressed={selected}
                          title={`Allow AI triage for ${tab.label} fallback mail`}
                          onClick={() => {
                            const next = selected
                              ? scopedTabs.filter((id) => id !== tab.id)
                              : [...scopedTabs, tab.id];
                            onChange({
                              ...policy,
                              ai_suggestible_gmail_scope: "selected",
                              ai_suggestible_gmail_categories: next,
                            });
                          }}
                          className={`rounded-full border px-2 py-0.5 text-xs transition disabled:opacity-40 ${
                            selected
                              ? "border-sky-700 bg-sky-950/60 text-sky-300"
                              : "border-zinc-700 text-zinc-500 hover:bg-zinc-800 hover:text-zinc-200"
                          }`}
                        >
                          {tab.label}
                        </button>
                      );
                    })}
                  </div>
                ) : null}
              </>
            ) : null}

            {autoProduceEnabled ? (
              <button
                onClick={() =>
                  onChange({ ...policy, auto_produce: !policy.auto_produce })
                }
                disabled={busy}
                aria-pressed={policy.auto_produce}
                title="When on, accepting an item automatically starts drafting — you can review the results in the queue. Off = draft manually."
                className={`self-start rounded-full border px-2 py-0.5 text-xs transition disabled:opacity-40 ${
                  policy.auto_produce
                    ? "border-amber-700 bg-amber-950/60 text-amber-300"
                    : "border-zinc-700 text-zinc-500 hover:bg-zinc-800 hover:text-zinc-200"
                }`}
              >
                ⚡ auto-draft {policy.auto_produce ? "on" : "off"}
              </button>
            ) : null}
          </div>
        </div>
      )}
    </div>
  );
}

function CategoryForm({
  initial,
  creating,
  saving,
  onSave,
  onCancel,
  agentLaunchEnabled,
}: {
  initial: Draft;
  creating: boolean;
  saving: boolean;
  onSave: (draft: Draft) => void;
  onCancel: () => void;
  agentLaunchEnabled: boolean;
}) {
  const [draft, setDraft] = useState<Draft>(initial);
  const [touched, setTouched] = useState(false);
  const errors = useMemo(
    () => validate(draft, creating),
    [draft, creating],
  );

  const update = (patch: Partial<Draft>) => {
    setDraft((d) => ({ ...d, ...patch }));
    setTouched(true);
  };

  const isPristine = !touched;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && isPristine) onCancel();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [isPristine, onCancel]);

  const inputCls =
    "rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1.5 text-sm text-zinc-200 focus:border-sky-600 focus:outline-none";

  return (
    <div className="surface-card surface-flat surface-body-zinc rounded-md border border-zinc-800 bg-zinc-950/60 p-3">
      <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
        <label className="flex flex-col gap-1 text-xs text-zinc-400">
          category_id
          <input
            className={`${inputCls} font-mono disabled:opacity-60`}
            value={draft.category_id}
            disabled={!creating}
            onChange={(e) => update({ category_id: e.target.value })}
            placeholder="e.g. call_log"
            title={
              creating
                ? "lowercase a-z, 0-9, underscore; max 64"
                : "id is fixed once created"
            }
          />
        </label>
        <label className="flex flex-col gap-1 text-xs text-zinc-400">
          display name
          <input
            className={inputCls}
            value={draft.display_name}
            onChange={(e) => update({ display_name: e.target.value })}
            placeholder="e.g. Call log"
          />
        </label>
        <label className="flex flex-col gap-1 text-xs text-zinc-400">
          color
          <input
            className="h-8 w-16 cursor-pointer rounded-md border border-zinc-700 bg-zinc-900 p-0.5"
            type="color"
            value={draft.color}
            onChange={(e) => update({ color: e.target.value })}
          />
        </label>
        <label className="flex flex-col gap-1 text-xs text-zinc-400">
          sort
          <input
            className={inputCls}
            type="number"
            value={draft.sort}
            onChange={(e) => update({ sort: e.target.value })}
          />
        </label>
      </div>
      <label className="mt-3 flex flex-col gap-1 text-xs text-zinc-400">
        Definition
        <textarea
          className={`${inputCls} w-full`}
          rows={3}
          value={draft.description}
          onChange={(e) => update({ description: e.target.value })}
          placeholder="What belongs in this category? Be specific about senders, subjects, and content."
        />
      </label>

      {agentLaunchEnabled ? (
        <div className="mt-3 grid grid-cols-1 gap-3 md:grid-cols-2">
          <label className="flex flex-col gap-1 text-xs text-zinc-400">
            Agent workdir
            <input
              className={`${inputCls} font-mono`}
              value={draft.default_agent_dir}
              onChange={(e) => update({ default_agent_dir: e.target.value })}
              placeholder="/home/example/projects/BusinessOS"
            />
          </label>
          <label className="flex flex-col gap-1 text-xs text-zinc-400">
            Agent context
            <textarea
              className={`${inputCls} min-h-20 w-full`}
              rows={3}
              value={draft.default_agent_context}
              onChange={(e) =>
                update({ default_agent_context: e.target.value })
              }
              maxLength={2000}
              placeholder="Default instructions for agents launched from this category."
            />
          </label>
        </div>
      ) : null}

      {touched && errors.length > 0 ? (
        <ul className="mt-2 list-inside list-disc text-xs text-amber-300">
          {errors.map((e, i) => (
            <li key={i}>{e}</li>
          ))}
        </ul>
      ) : null}

      <div className="mt-3 flex items-center gap-2">
        <Button
          variant="primary"
          size="md"
          busy={saving}
          disabled={errors.length > 0}
          onClick={() => onSave(draft)}
        >
          {saving ? "Saving…" : creating ? "Create category" : "Save changes"}
        </Button>
        <Button variant="ghost" size="md" onClick={onCancel}>
          Cancel
        </Button>
      </div>
    </div>
  );
}

export default function Categories({
  onUnauthorized,
  helpTopicId,
  onOpenHelpTopic,
  autoProduceEnabled,
  aiTriageEnabled,
  agentLaunchEnabled,
}: {
  onUnauthorized: () => void;
  helpTopicId?: string;
  onOpenHelpTopic: (topicId: string) => void;
  autoProduceEnabled: boolean;
  aiTriageEnabled: boolean;
  agentLaunchEnabled: boolean;
}) {
  const { categories, refresh } = useCategories();
  const [creating, setCreating] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ text: string; kind: "error" | "info" } | null>(null);
  const [policies, setPolicies] = useState<Record<string, WorkQueuePolicy>>(
    {},
  );
  const [policyBusyId, setPolicyBusyId] = useState<string | null>(null);
  const packetKindCatalog = usePacketKinds();
  const [confirmDeleteRecord, setConfirmDeleteRecord] =
    useState<CategoryRecord | null>(null);

  const loadPolicies = useCallback(async () => {
    try {
      const res = await api.workQueuePolicies();
      const nextPolicies: Record<string, WorkQueuePolicy> = {};
      const repairs: WorkQueuePolicy[] = [];
      for (const policy of res.policies) {
        const normalized = normalizeWorkQueuePolicy(policy);
        nextPolicies[policy.category_id] = normalized;
        if (normalized !== policy) repairs.push(normalized);
      }
      setPolicies(nextPolicies);
      if (repairs.length > 0) {
        await Promise.all(
          repairs.map((policy) =>
            api.upsertWorkQueuePolicy({
              policy,
              idempotency_key: crypto.randomUUID(),
              actor_id: null,
            }),
          ),
        );
      }
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice({ text: `Failed to load work-queue policies: ${errorMessage(err)}`, kind: "error" });
    }
  }, [onUnauthorized]);

  useAppCommand("refresh", () => {
    void refresh();
    void loadPolicies();
  });
  useAppCommand("categories.new", () => {
    setEditingId(null);
    setCreating(true);
  });

  useEffect(() => {
    void loadPolicies();
  }, [loadPolicies]);

  const savePolicy = async (policy: WorkQueuePolicy) => {
    const normalized = normalizeWorkQueuePolicy(policy);
    setPolicyBusyId(policy.category_id);
    setNotice(null);
    // Snapshot the current value (may be undefined if no explicit policy row yet)
    const snapshot = policies[policy.category_id];
    // Optimistically write the new policy immediately
    setPolicies((prev) => ({ ...prev, [policy.category_id]: normalized }));
    try {
      await api.upsertWorkQueuePolicy({
        policy: normalized,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      // Silent reconciliation — no await, no flash
      void loadPolicies();
    } catch (err) {
      if (isUnauthorized(err)) {
        onUnauthorized();
      } else {
        // Revert: restore the snapshot (delete the key if it was previously absent)
        setPolicies((prev) => {
          const next = { ...prev };
          if (snapshot === undefined) {
            delete next[policy.category_id];
          } else {
            next[policy.category_id] = snapshot;
          }
          return next;
        });
        setNotice({ text: `Policy save failed: ${errorMessage(err)}`, kind: "error" });
      }
    } finally {
      setPolicyBusyId(null);
    }
  };

  const nextSort =
    categories.length > 0
      ? Math.max(...categories.map((c) => c.sort)) + 10
      : 0;

  const closeForm = () => {
    setCreating(false);
    setEditingId(null);
  };

  const save = async (draft: Draft) => {
    setSaving(true);
    setNotice(null);
    try {
      await api.upsertCategory({
        category: {
          category_id: draft.category_id.trim(),
          display_name: draft.display_name.trim(),
          description: draft.description.trim(),
          color: draft.color,
          sort: Number(draft.sort),
          is_system: draft.is_system,
          default_agent_dir: draft.default_agent_dir.trim(),
          default_agent_context: draft.default_agent_context.trim(),
        },
        policy: null,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      closeForm();
      await refresh();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice({ text: `Save failed: ${errorMessage(err)}`, kind: "error" });
    } finally {
      setSaving(false);
    }
  };

  const remove = async (record: CategoryRecord) => {
    setBusyId(record.category_id);
    setNotice(null);
    try {
      await api.deleteCategory(record.category_id, {
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      if (editingId === record.category_id) closeForm();
      await refresh();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice({ text: deleteErrorMessage(err), kind: "error" });
    } finally {
      setBusyId(null);
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <div className="surface-section-head surface-head-zinc flex items-center justify-between">
        <div className="flex items-center gap-2">
          <h2 className="text-lg font-semibold text-zinc-100">Categories</h2>
          <SectionHelpButton
            topicId={helpTopicId}
            onOpenHelp={onOpenHelpTopic}
            label="Open help for Categories"
          />
        </div>
        <Button
          variant="primary"
          size="md"
          onClick={() => {
            setEditingId(null);
            setCreating(true);
          }}
        >
          + New category
        </Button>
      </div>

      <p className="text-sm text-zinc-400">
        {aiTriageEnabled
          ? "Rules sort messages into categories; anything unmatched is sorted automatically by AI using these definitions. Edit a definition to change how AI files mail."
          : "Rules sort messages into categories; anything unmatched goes to the default category. Edit a definition to change how mail is filed."}
      </p>

      {notice ? (
        <div
          className={`rounded-md border px-3 py-2 text-sm ${
            notice.kind === "error"
              ? "border-red-900/60 bg-red-950/40 text-red-300"
              : "border-amber-900/60 bg-amber-950/30 text-amber-300"
          }`}
        >
          {notice.text}
        </div>
      ) : null}

      {creating ? (
        <CategoryForm
          key="new"
          initial={emptyDraft(nextSort)}
          creating
          saving={saving}
          onSave={(d) => void save(d)}
          onCancel={closeForm}
          agentLaunchEnabled={agentLaunchEnabled}
        />
      ) : null}

      {categories.length === 0 ? (
        <EmptyState
          title="No categories."
          action={
            <Button
              variant="primary"
              size="sm"
              onClick={() => {
                setEditingId(null);
                setCreating(true);
              }}
            >
              Create category
            </Button>
          }
        >
          Defaults are loaded on first use — if this persists, check your
          connection.
        </EmptyState>
      ) : (
        <div className={`${tableWrapCls} surface-flat surface-body-zinc`}>
          <table className={tableCls}>
            <thead className={`${theadCls} surface-head-zinc`}>
              <tr>
                <th className={cellCls}></th>
                <th className={cellCls}>Id</th>
                <th className={cellCls}>Display name</th>
                <th className={cellCls}>Definition</th>
                <th className={cellCls}>Sort</th>
                <th
                  className={`cursor-help ${cellCls}`}
                  title="When enabled, emails in this category create work items in the queue. Existing email is added within about 2 minutes. Draft types are configured per category using the output settings below."
                >
                  Work items
                </th>
                <th className={cellCls}></th>
                <th className={cellCls}></th>
              </tr>
            </thead>
            <tbody className={rowDivideCls}>
              {categories.map((c) => {
                const busy = busyId === c.category_id;
                const editing = editingId === c.category_id;
                const policy = normalizeWorkQueuePolicy(
                  policies[c.category_id] ??
                    defaultWorkQueuePolicy(c.category_id),
                );
                const policyBusy = policyBusyId === c.category_id;
                return (
                  <Fragment key={c.category_id}>
                    <tr className={rowHoverCls}>
                      <td className={cellCls}>
                        <span
                          className="inline-block h-3.5 w-3.5 rounded-full border border-zinc-700"
                          style={{ backgroundColor: c.color }}
                          title={c.color}
                        />
                      </td>
                      <td className={`${cellCls} font-mono text-zinc-200`}>
                        {c.category_id}
                      </td>
                      <td className={`${cellCls} text-zinc-200`}>
                        {c.display_name}
                      </td>
                      <td className={`max-w-md truncate ${cellCls} text-xs text-zinc-400`}>
                        {c.description || "—"}
                      </td>
                      <td className={`${cellCls} text-right tabular-nums font-mono text-zinc-400`}>
                        {c.sort}
                      </td>
                      <td className={cellCls}>
                        <div className="flex items-start gap-2">
                          <button
                            onClick={() =>
                              void savePolicy({
                                ...policy,
                                create_work_item: !policy.create_work_item,
                              })
                            }
                            disabled={policyBusy}
                            role="switch"
                            aria-checked={policy.create_work_item}
                            aria-label={
                              policy.create_work_item
                                ? "Stop creating work items for this category"
                                : "Create work items from emails classified into this category"
                            }
                            className={`relative mt-0.5 inline-flex h-5 w-9 shrink-0 items-center rounded-full transition disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70 ${
                              policy.create_work_item
                                ? "bg-[var(--success)]"
                                : "bg-[var(--toggle-off)]"
                            }`}
                            title={
                              policy.create_work_item
                                ? "Stop creating work items for this category"
                                : "Create work items from emails in this category — existing email is added within about 2 minutes."
                            }
                          >
                            <span
                              className={`inline-block h-4 w-4 transform rounded-full bg-white transition ${
                                policy.create_work_item
                                  ? "translate-x-4.5"
                                  : "translate-x-0.5"
                              }`}
                            />
                          </button>
                          {policy.create_work_item ? (
                            <PacketKindPicker
                              policy={policy}
                              catalog={packetKindCatalog}
                              onChange={(p) => void savePolicy(p)}
                              busy={policyBusy}
                              autoProduceEnabled={autoProduceEnabled}
                              aiTriageEnabled={aiTriageEnabled}
                            />
                          ) : null}
                        </div>
                      </td>
                      <td className={cellCls}>
                        {c.is_system ? (
                          <span className="rounded-full bg-zinc-800 px-2 py-0.5 text-xs text-zinc-400 ring-1 ring-inset ring-zinc-700">
                            system
                          </span>
                        ) : null}
                      </td>
                      <td className={`whitespace-nowrap ${cellCls} text-right`}>
                        <Button
                          variant="secondary"
                          size="sm"
                          disabled={busy}
                          className="mr-2"
                          onClick={() => {
                            setCreating(false);
                            setEditingId(editing ? null : c.category_id);
                          }}
                        >
                          {editing ? "Close" : "Edit"}
                        </Button>
                        <Button
                          variant="danger"
                          size="sm"
                          disabled={busy || c.is_system}
                          title={
                            c.is_system
                              ? "System category can't be deleted"
                              : "Delete category"
                          }
                          onClick={() => setConfirmDeleteRecord(c)}
                        >
                          Delete
                        </Button>
                      </td>
                    </tr>
                    {editing ? (
                      <tr>
                        <td colSpan={8} className="bg-zinc-950/40 px-3 py-3">
                          <CategoryForm
                            key={c.category_id}
                            initial={draftFromRecord(c)}
                            creating={false}
                            saving={saving}
                            onSave={(d) => void save(d)}
                            onCancel={closeForm}
                            agentLaunchEnabled={agentLaunchEnabled}
                          />
                        </td>
                      </tr>
                    ) : null}
                  </Fragment>
                );
              })}
            </tbody>
          </table>
        </div>
      )}

      <ConfirmDialog
        open={confirmDeleteRecord !== null}
        title={`Delete category "${confirmDeleteRecord?.display_name}"?`}
        body={`"${confirmDeleteRecord?.category_id}" will be removed. Any rules pinned to it must be repointed or deleted. This cannot be undone.`}
        confirmLabel="Delete category"
        onConfirm={() => {
          if (!confirmDeleteRecord) return;
          const record = confirmDeleteRecord;
          setConfirmDeleteRecord(null);
          void remove(record);
        }}
        onCancel={() => setConfirmDeleteRecord(null)}
      />
    </div>
  );
}
