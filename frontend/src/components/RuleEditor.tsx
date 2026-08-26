import { useEffect, useMemo, useRef, useState } from "react";
import type { CategoryRecord } from "../types/generated/CategoryRecord";
import type { EmailTriageCondition } from "../types/generated/EmailTriageCondition";
import type { EmailTriageConditionCatalogItem } from "../types/generated/EmailTriageConditionCatalogItem";
import type { EmailTriageConditionCatalogResponse } from "../types/generated/EmailTriageConditionCatalogResponse";
import type { EmailTriageConditionId } from "../types/generated/EmailTriageConditionId";
import type { EmailTriageConditionOperator } from "../types/generated/EmailTriageConditionOperator";
import type { EmailTriageConditionV2 } from "../types/generated/EmailTriageConditionV2";
import type { EmailTriageConditionValue } from "../types/generated/EmailTriageConditionValue";
import type { EmailTriageField } from "../types/generated/EmailTriageField";
import type { EmailTriageGmailCategory } from "../types/generated/EmailTriageGmailCategory";
import type { EmailTriageMatchMode } from "../types/generated/EmailTriageMatchMode";
import type { EmailTriageRule } from "../types/generated/EmailTriageRule";
import type { PacketKindRecord } from "../types/generated/PacketKindRecord";
import type { WorkQueuePolicy } from "../types/generated/WorkQueuePolicy";
import {
  ApiError,
  api,
  errorMessage,
  isRevisionConflict,
  isUnauthorized,
} from "../lib/api";
import {
  CATEGORY_COLORS,
  CATEGORY_ID_PATTERN,
  DEFAULT_CATEGORY_COLOR,
  FALLBACK_CATEGORY_ID,
  categoryLabel,
  defaultWorkQueuePolicy,
  normalizeWorkQueuePolicy,
  nextCategorySort,
  slugifyCategoryId,
  useCategories,
} from "../lib/categories";
import { usePacketKinds } from "../lib/packetKinds";
import { Button } from "./ui";

const DEFAULT_CONDITION_ID: EmailTriageConditionId = "message.subject";
const DEFAULT_CONDITION_OP: EmailTriageConditionOperator = "contains";
const MATCH_MODES: EmailTriageMatchMode[] = ["all", "any"];
const AI_SUGGEST_ALL = "*";
const GMAIL_TABS: { id: EmailTriageGmailCategory; label: string }[] = [
  { id: "primary", label: "Primary" },
  { id: "updates", label: "Updates" },
  { id: "social", label: "Social" },
  { id: "promotions", label: "Promotions" },
  { id: "forums", label: "Forums" },
];

const LEGACY_FIELD_TO_CONDITION_ID: Record<
  EmailTriageField,
  EmailTriageConditionId
> = {
  label: "message.label",
  from: "message.from",
  to: "message.to",
  subject: "message.subject",
  body: "message.body",
  header: "message.header",
  sender_in_crm_contacts: "crm.sender_contact.exists",
  sender_domain_in_crm_companies: "crm.sender_company.exists",
};

interface ConditionDraft {
  condition_id: EmailTriageConditionId;
  op: EmailTriageConditionOperator;
  value: string;
  header_name: string;
}

interface Draft {
  rule_id: string;
  priority: string;
  match_mode: EmailTriageMatchMode;
  pinned_category: string;
  enabled: boolean;
  conditions: ConditionDraft[];
}

function emptyCondition(): ConditionDraft {
  return {
    condition_id: DEFAULT_CONDITION_ID,
    op: DEFAULT_CONDITION_OP,
    value: "",
    header_name: "",
  };
}

function emptyDraft(): Draft {
  return {
    rule_id: "",
    priority: "100",
    match_mode: "all",
    pinned_category: "",
    enabled: true,
    conditions: [emptyCondition()],
  };
}

interface CategoryCreateDraft {
  category_id: string;
  display_name: string;
  description: string;
  color: string;
  policy: WorkQueuePolicy;
}

function CategoryPicker({
  categories,
  value,
  inputCls,
  onSelect,
  onCreate,
  packetKindCatalog,
  aiTriageEnabled,
}: {
  categories: CategoryRecord[];
  value: string;
  inputCls: string;
  onSelect: (categoryId: string) => void;
  onCreate: (draft: CategoryCreateDraft) => Promise<CategoryRecord>;
  packetKindCatalog: PacketKindRecord[];
  aiTriageEnabled: boolean;
}) {
  const categoryBeforeCreateRef = useRef(value);
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const [categoryId, setCategoryId] = useState("");
  const [description, setDescription] = useState("");
  const [color, setColor] = useState(DEFAULT_CATEGORY_COLOR);
  const [policy, setPolicy] = useState<WorkQueuePolicy>(() =>
    newCategoryPolicy(""),
  );
  const [saving, setSaving] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);

  const categoryIdDuplicate = categories.some(
    (category) => category.category_id === categoryId.trim(),
  );
  const createValid =
    name.trim().length > 0 &&
    CATEGORY_ID_PATTERN.test(categoryId.trim()) &&
    !categoryIdDuplicate;

  const startCreate = () => {
    categoryBeforeCreateRef.current = value;
    setName("");
    setCategoryId("");
    setDescription("");
    setColor(DEFAULT_CATEGORY_COLOR);
    setPolicy(newCategoryPolicy(""));
    setCreateError(null);
    setCreating(true);
    onSelect("");
  };

  const submitCreate = async () => {
    if (!createValid) return;
    setSaving(true);
    setCreateError(null);
    try {
      const created = await onCreate({
        category_id: categoryId.trim(),
        display_name: name.trim(),
        description: description.trim(),
        color,
        policy: { ...policy, category_id: categoryId.trim() },
      });
      onSelect(created.category_id);
      categoryBeforeCreateRef.current = created.category_id;
      setCreating(false);
    } catch (err) {
      setCreateError(errorMessage(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="flex flex-col gap-1">
      <select
        className={inputCls}
        value={value}
        disabled={creating}
        aria-label="Category"
        onChange={(event) => onSelect(event.target.value)}
      >
        <option value="">Choose an existing category…</option>
        {categories.map((category) => (
          <option key={category.category_id} value={category.category_id}>
            {categoryLabel(category)} ({category.category_id})
          </option>
        ))}
      </select>
      {!creating ? (
        <button
          type="button"
          className="w-fit text-xs font-medium text-sky-400 transition hover:text-sky-300"
          onClick={startCreate}
        >
          + Create new category for this rule
        </button>
      ) : null}

      {creating ? (
        <div className="mt-2 rounded-md border border-sky-900/70 bg-sky-950/20 p-3">
          <div className="mb-3 text-sm font-medium text-zinc-200">
            New category
          </div>
          <div className="grid gap-2 sm:grid-cols-2">
            <label className="flex flex-col gap-1 text-xs text-zinc-400">
              Name
              <input
                className={inputCls}
                value={name}
                onChange={(event) => {
                  const nextName = event.target.value;
                  setName(nextName);
                  const nextId = slugifyCategoryId(nextName);
                  setCategoryId(nextId);
                  setPolicy((current) => ({
                    ...current,
                    category_id: nextId,
                  }));
                }}
              />
            </label>
            <label className="flex flex-col gap-1 text-xs text-zinc-400">
              Category ID
              <input
                className={inputCls}
                value={categoryId}
                onChange={(event) => {
                  const nextId = event.target.value;
                  setCategoryId(nextId);
                  setPolicy((current) => ({
                    ...current,
                    category_id: nextId,
                  }));
                }}
              />
            </label>
          </div>
          <label className="mt-2 flex flex-col gap-1 text-xs text-zinc-400">
            Description
            <input
              className={inputCls}
              value={description}
              onChange={(event) => setDescription(event.target.value)}
              placeholder="Short meaning for unmatched mail"
            />
          </label>
          <div className="mt-2 flex flex-wrap items-center gap-2">
            {CATEGORY_COLORS.map((candidate) => (
              <button
                key={candidate}
                type="button"
                aria-label={`Use color ${candidate}`}
                className={`h-6 w-6 rounded-full border-2 transition ${
                  color === candidate
                    ? "border-zinc-100 ring-2 ring-sky-500/60"
                    : "border-zinc-700 hover:border-zinc-400"
                }`}
                style={{ backgroundColor: candidate }}
                onClick={() => setColor(candidate)}
              />
            ))}
          </div>
          <div className="mt-4 border-t border-zinc-800 pt-3">
            <div className="mb-2 text-xs font-semibold uppercase tracking-wide text-zinc-400">
              Work items for this new category
            </div>
            <OutputPicker
              policy={policy}
              catalog={packetKindCatalog}
              busy={saving}
              onChange={setPolicy}
              aiTriageEnabled={aiTriageEnabled}
            />
          </div>
          {categoryId.trim().length > 0 &&
          !CATEGORY_ID_PATTERN.test(categoryId.trim()) ? (
            <div className="mt-2 text-xs text-amber-300">
              Category ID must use lowercase letters, numbers, or underscores.
            </div>
          ) : null}
          {categoryIdDuplicate ? (
            <div className="mt-2 text-xs text-amber-300">
              Category ID already exists.
            </div>
          ) : null}
          {createError ? (
            <div className="mt-2 text-xs text-red-300">
              Category save failed: {createError}
            </div>
          ) : null}
          <div className="mt-3 flex items-center gap-2">
            <Button
              variant="primary"
              size="sm"
              busy={saving}
              disabled={!createValid}
              onClick={submitCreate}
            >
              {saving ? "Creating..." : "Create category"}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                setCreating(false);
                setCreateError(null);
                onSelect(categoryBeforeCreateRef.current);
              }}
            >
              Cancel
            </Button>
          </div>
        </div>
      ) : null}
    </div>
  );
}

function catalogItems(
  catalog: EmailTriageConditionCatalogResponse | null,
): EmailTriageConditionCatalogItem[] {
  return catalog?.groups.flatMap((group) => group.items) ?? [];
}

function catalogItemFor(
  catalog: EmailTriageConditionCatalogResponse | null,
  conditionId: EmailTriageConditionId,
): EmailTriageConditionCatalogItem | null {
  return (
    catalogItems(catalog).find((item) => item.condition_id === conditionId) ??
    null
  );
}

function conditionValueText(value: EmailTriageConditionValue): {
  value: string;
  headerName: string;
} {
  if (typeof value === "string") return { value: "", headerName: "" };
  if ("text" in value) return { value: value.text, headerName: "" };
  if ("header" in value) {
    return { value: value.header.value, headerName: value.header.name };
  }
  if ("bool" in value) return { value: String(value.bool), headerName: "" };
  if ("number" in value) return { value: String(value.number), headerName: "" };
  if ("money_cents" in value) {
    return { value: String(value.money_cents), headerName: "" };
  }
  if ("date" in value) return { value: value.date, headerName: "" };
  if ("string_list" in value) {
    return { value: value.string_list.join(", "), headerName: "" };
  }
  return { value: "", headerName: "" };
}

function conditionDraftFromV2(condition: EmailTriageConditionV2): ConditionDraft {
  const parsed = conditionValueText(condition.value);
  return {
    condition_id: condition.condition_id,
    op: condition.op,
    value: parsed.value,
    header_name: parsed.headerName,
  };
}

function legacyConditionToDraft(condition: EmailTriageCondition): ConditionDraft {
  const conditionId =
    LEGACY_FIELD_TO_CONDITION_ID[condition.field] ?? DEFAULT_CONDITION_ID;
  return {
    condition_id: conditionId,
    op:
      (condition.field === "sender_in_crm_contacts" ||
        condition.field === "sender_domain_in_crm_companies") &&
      condition.op === "exists"
        ? "is_true"
        : (condition.op as EmailTriageConditionOperator),
    value: condition.value,
    header_name: condition.header_name ?? "",
  };
}

function opRequiresValue(op: EmailTriageConditionOperator): boolean {
  return op !== "exists" && op !== "is_true" && op !== "is_false";
}

function defaultValueFor(item: EmailTriageConditionCatalogItem): {
  value: string;
  headerName: string;
} {
  if (item.value_kind === "header") return { value: "", headerName: "" };
  return { value: "", headerName: "" };
}

function draftValueToCondition(
  condition: ConditionDraft,
  item: EmailTriageConditionCatalogItem | null,
): EmailTriageConditionValue {
  if (item?.value_kind === "header") {
    return {
      header: {
        name: condition.header_name.trim(),
        value: opRequiresValue(condition.op) ? condition.value : "",
      },
    };
  }
  if (condition.op === "in") {
    return {
      string_list: condition.value
        .split(",")
        .map((value) => value.trim())
        .filter((value) => value.length > 0),
    };
  }
  if (!opRequiresValue(condition.op)) return "empty";
  return { text: condition.value };
}

function conditionSummary(
  condition: ConditionDraft,
  item: EmailTriageConditionCatalogItem | null,
): string {
  const label = item?.label ?? "Condition";
  if (!opRequiresValue(condition.op)) return `${label} ${opLabel(condition.op)}`;
  if (item?.value_kind === "header") {
    return `${label} ${condition.header_name} ${opLabel(condition.op)} "${condition.value}"`;
  }
  return `${label} ${opLabel(condition.op)} "${condition.value}"`;
}

function opLabel(op: EmailTriageConditionOperator): string {
  switch (op) {
    case "contains":
      return "contains";
    case "equals":
      return "is";
    case "starts_with":
      return "starts with";
    case "regex":
      return "matches";
    case "exists":
      return "is known";
    case "is_true":
      return "is true";
    case "is_false":
      return "is not true";
    case "in":
      return "is one of";
    case "greater_than":
      return "is greater than";
    case "less_than":
      return "is less than";
    case "at_least":
      return "is at least";
    case "at_most":
      return "is at most";
  }
}

function ConditionPicker({
  catalog,
  value,
  inputCls,
  onSelect,
}: {
  catalog: EmailTriageConditionCatalogResponse | null;
  value: EmailTriageConditionId;
  inputCls: string;
  onSelect: (conditionId: EmailTriageConditionId) => void;
}) {
  const selected = catalogItemFor(catalog, value);
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const normalizedQuery = query.trim().toLowerCase();
  const groups =
    catalog?.groups
      .map((group) => ({
        ...group,
        items: group.items.filter((item) => {
          if (!normalizedQuery) return true;
          return (
            item.label.toLowerCase().includes(normalizedQuery) ||
            item.description.toLowerCase().includes(normalizedQuery) ||
            item.condition_id.toLowerCase().includes(normalizedQuery)
          );
        }),
      }))
      .filter((group) => group.items.length > 0) ?? [];

  return (
    <div
      className="relative"
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) {
          setOpen(false);
          setQuery("");
        }
      }}
    >
      <button
        type="button"
        className={`${inputCls} flex w-full items-center justify-between gap-2 text-left`}
        title={selected?.description ?? "Choose a condition"}
        onClick={() => setOpen((next) => !next)}
      >
        <span className="min-w-0 truncate">
          {selected?.label ?? "Loading conditions"}
        </span>
        <span className="text-zinc-500">⌄</span>
      </button>
      {open ? (
        <div className="absolute left-0 right-0 top-full z-30 mt-1 overflow-hidden rounded-md border border-zinc-700 bg-zinc-950 shadow-xl">
          <div className="border-b border-zinc-800 p-2">
            <input
              className={`${inputCls} w-full`}
              value={query}
              autoFocus
              onChange={(event) => setQuery(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Escape") setOpen(false);
              }}
              placeholder="Search filters"
            />
          </div>
          <div className="max-h-[min(24rem,calc(100vh-12rem))] overflow-y-auto py-1">
            {groups.map((group) => (
              <div key={group.group}>
                <div className="px-3 py-1 text-[11px] font-semibold uppercase tracking-wide text-zinc-500">
                  {group.label}
                </div>
                {group.items.map((item) => (
                  <button
                    key={item.condition_id}
                    type="button"
                    className={`block w-full px-3 py-2 text-left text-sm transition hover:bg-zinc-900 ${
                      item.condition_id === value
                        ? "text-sky-300"
                        : "text-zinc-200"
                    }`}
                    onMouseDown={(event) => event.preventDefault()}
                    onClick={() => {
                      onSelect(item.condition_id);
                      setOpen(false);
                      setQuery("");
                    }}
                  >
                    <span className="block truncate">{item.label}</span>
                    <span className="mt-0.5 block truncate text-xs text-zinc-500">
                      {item.description}
                    </span>
                  </button>
                ))}
              </div>
            ))}
            {catalog && groups.length === 0 ? (
              <div className="px-3 py-3 text-sm text-zinc-500">
                No matching filters
              </div>
            ) : null}
          </div>
        </div>
      ) : null}
    </div>
  );
}

function matchModeLabel(mode: EmailTriageMatchMode): string {
  return mode === "any" ? "any" : "all";
}

function providerLabel(provider: string | null): string {
  if (provider === "invoice_ninja") return "Invoice Ninja";
  if (provider === "stripe") return "Stripe";
  if (provider === "qbo") return "QuickBooks";
  return "configured accounting";
}

function conditionSourceLabel(
  conditionId: EmailTriageConditionId,
  catalog: EmailTriageConditionCatalogResponse | null,
  accountingProvider: string | null,
): string {
  const item = catalogItemFor(catalog, conditionId);
  if (item?.provider_dependency === "crm") return "Configured CRM";
  if (conditionId.startsWith("accounting.")) {
    return `${providerLabel(accountingProvider)} snapshots`;
  }
  if (conditionId.startsWith("workflow.")) return "BusinessOS work queue";
  if (conditionId.startsWith("message.") || conditionId.startsWith("source.")) {
    return "Email metadata";
  }
  return "BusinessOS facts";
}

function OutputPicker({
  policy,
  catalog,
  busy,
  onChange,
  aiTriageEnabled,
}: {
  policy: WorkQueuePolicy;
  catalog: PacketKindRecord[];
  busy: boolean;
  onChange: (policy: WorkQueuePolicy) => void;
  aiTriageEnabled: boolean;
}) {
  const aiOn = policy.ai_suggestible_packet_kinds.length > 0;
  const canScopeAiTabs = policy.category_id === FALLBACK_CATEGORY_ID;
  const aiScope = policy.ai_suggestible_gmail_scope;
  const scopedTabs =
    aiScope === "selected" || aiScope === "default"
      ? policy.ai_suggestible_gmail_categories
      : [];
  return (
    <div className="rounded-md border border-zinc-800 bg-zinc-950/60 p-3">
      <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <label className="flex cursor-pointer items-center gap-2 text-sm text-zinc-200">
          <input
            type="checkbox"
            checked={policy.create_work_item}
            disabled={busy}
            onChange={(e) =>
              onChange({ ...policy, create_work_item: e.target.checked })
            }
            className="h-4 w-4 rounded border-zinc-600 bg-zinc-950 text-sky-600 focus:ring-1 focus:ring-sky-600"
          />
          Create work items for this category
        </label>
        <label className="flex cursor-pointer items-center gap-2 text-xs text-zinc-400">
          <input
            type="checkbox"
            checked={policy.auto_produce}
            disabled={busy || !policy.create_work_item}
            onChange={(e) =>
              onChange({ ...policy, auto_produce: e.target.checked })
            }
            className="h-4 w-4 rounded border-zinc-600 bg-zinc-950 text-amber-600 focus:ring-1 focus:ring-amber-600"
          />
          Auto-draft after accept
        </label>
      </div>

      <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
        {catalog.map((kind) => {
          const checked = policy.packet_kinds.includes(kind.kind_id);
          return (
            <label
              key={kind.kind_id}
              className={`flex cursor-pointer items-start gap-2 rounded border px-2 py-2 text-sm ${
                checked
                  ? "border-sky-800 bg-sky-950/30 text-sky-100"
                  : "border-zinc-800 bg-zinc-900/40 text-zinc-300"
              }`}
              title={kind.description}
            >
              <input
                type="checkbox"
                checked={checked}
                disabled={busy || !policy.create_work_item}
                onChange={(e) =>
                  onChange({
                    ...policy,
                    packet_kinds: e.target.checked
                      ? [...policy.packet_kinds, kind.kind_id]
                      : policy.packet_kinds.filter((id) => id !== kind.kind_id),
                  })
                }
                className="mt-0.5 h-4 w-4 rounded border-zinc-600 bg-zinc-950 text-sky-600 focus:ring-1 focus:ring-sky-600"
              />
              <span className="min-w-0">
                <span className="block truncate font-medium">{kind.title}</span>
                <span className="block truncate text-xs text-zinc-500">
                  {kind.produce_available ? "Draft available" : "Coming soon"}
                </span>
              </span>
            </label>
          );
        })}
      </div>

      {aiTriageEnabled ? (
        <label className="mt-3 flex cursor-pointer items-center gap-2 text-xs text-zinc-400">
          <input
            type="checkbox"
            checked={aiOn}
            disabled={busy || !policy.create_work_item}
            onChange={(e) =>
              onChange({
                ...policy,
                ai_suggestible_packet_kinds: e.target.checked
                  ? [AI_SUGGEST_ALL]
                  : [],
                ai_suggestible_gmail_scope: e.target.checked
                  ? policy.ai_suggestible_gmail_scope === "default" &&
                    policy.ai_suggestible_gmail_categories.length === 0
                    ? "all"
                    : policy.ai_suggestible_gmail_scope
                  : "default",
                ai_suggestible_gmail_categories: e.target.checked
                  ? policy.ai_suggestible_gmail_categories
                  : [],
              })
            }
            className="h-4 w-4 rounded border-zinc-600 bg-zinc-950 text-violet-600 focus:ring-1 focus:ring-violet-600"
          />
          Let AI add extra draft types when a specific message warrants them
        </label>
      ) : null}
      {aiTriageEnabled && aiOn && canScopeAiTabs ? (
        <div className="mt-2 flex flex-wrap items-center gap-1.5">
          <span className="text-xs text-zinc-500">AI triage tabs</span>
          <button
            type="button"
            disabled={busy || !policy.create_work_item}
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
                disabled={busy || !policy.create_work_item}
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
    </div>
  );
}

function draftFromRule(rule: EmailTriageRule): Draft {
  return {
    rule_id: rule.rule_id,
    priority: String(rule.priority),
    match_mode: rule.match_mode,
    pinned_category: rule.pinned_category,
    enabled: rule.enabled,
    conditions:
      rule.conditions_v2.length > 0
        ? rule.conditions_v2.map(conditionDraftFromV2)
        : rule.conditions.map(legacyConditionToDraft),
  };
}

function draftForNewRule(seed?: EmailTriageRule | null): Draft {
  return seed
    ? { ...draftFromRule(seed), pinned_category: "" }
    : emptyDraft();
}

function validate(
  draft: Draft,
  catalog: EmailTriageConditionCatalogResponse | null,
): string[] {
  const errors: string[] = [];
  const items = catalogItems(catalog);
  if (draft.rule_id.trim().length === 0) errors.push("Rule ID is required.");
  if (draft.pinned_category.length === 0) {
    errors.push("Category is required.");
  }
  const priority = Number(draft.priority);
  if (!Number.isInteger(priority)) {
    errors.push("priority must be an integer.");
  }
  if (draft.conditions.length === 0) {
    errors.push("at least one condition is required.");
  }
  draft.conditions.forEach((c, i) => {
    const item = items.find((candidate) => candidate.condition_id === c.condition_id);
    if (!item) {
      errors.push(`condition ${i + 1}: choose a supported condition.`);
      return;
    }
    if (!item.supported_ops.includes(c.op)) {
      errors.push(`condition ${i + 1}: choose a supported operator.`);
    }
    if (item.value_kind === "header" && c.header_name.trim().length === 0) {
      errors.push(
        `condition ${i + 1}: header name is required.`,
      );
    }
    if (
      opRequiresValue(c.op) &&
      item.value_kind !== "empty" &&
      item.value_kind !== "bool" &&
      c.value.trim().length === 0
    ) {
      errors.push(`condition ${i + 1}: value is required.`);
    }
  });
  return errors;
}

function draftToRule(
  draft: Draft,
  catalog: EmailTriageConditionCatalogResponse | null,
): EmailTriageRule {
  const items = catalogItems(catalog);
  return {
    rule_id: draft.rule_id.trim(),
    priority: Number(draft.priority),
    match_mode: draft.match_mode,
    pinned_category: draft.pinned_category,
    enabled: draft.enabled,
    conditions: [],
    conditions_v2: draft.conditions.map((condition): EmailTriageConditionV2 => {
      const item =
        items.find((candidate) => candidate.condition_id === condition.condition_id) ??
        null;
      return {
        condition_id: condition.condition_id,
        op: condition.op,
        value: draftValueToCondition(condition, item),
      };
    }),
  };
}

function newCategoryPolicy(categoryId: string): WorkQueuePolicy {
  return {
    ...defaultWorkQueuePolicy(categoryId),
    create_work_item: true,
  };
}

export interface RuleEditorProps {
  editing: { rule: EmailTriageRule; revision: number } | null;
  seed?: EmailTriageRule | null;
  previewSummary?: {
    matched: number;
    total: number;
    loading: boolean;
  } | null;
  onSaved: () => void;
  onCancel: () => void;
  onUnauthorized: () => void;
  onConflict: () => void;
  onDraftChange: (rule: EmailTriageRule | null, dirty: boolean) => void;
  onTestDraft: (rule: EmailTriageRule) => void;
  aiTriageEnabled: boolean;
}

export default function RuleEditor({
  editing,
  seed,
  previewSummary,
  onSaved,
  onCancel,
  onUnauthorized,
  onConflict,
  onDraftChange,
  onTestDraft,
  aiTriageEnabled,
}: RuleEditorProps) {
  const { categories, refresh: refreshCategories } = useCategories();
  const packetKindCatalog = usePacketKinds();
  const [draft, setDraft] = useState<Draft>(() =>
    editing
      ? draftFromRule(editing.rule)
      : draftForNewRule(seed),
  );
  const [dirty, setDirty] = useState(false);
  const [ruleIdTouched, setRuleIdTouched] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState(false);
  const [conditionCatalog, setConditionCatalog] =
    useState<EmailTriageConditionCatalogResponse | null>(null);
  const autoTestedSeedRef = useRef<EmailTriageRule | null>(null);
  const [accountingProvider, setAccountingProvider] = useState<string | null>(
    null,
  );

  useEffect(() => {
    let cancelled = false;
    api
      .conditionCatalog()
      .then((catalog) => {
        if (!cancelled) setConditionCatalog(catalog);
      })
      .catch((err) => {
        if (isUnauthorized(err)) onUnauthorized();
        else setError(errorMessage(err));
      });
    return () => {
      cancelled = true;
    };
  }, [onUnauthorized]);

  useEffect(() => {
    let cancelled = false;
    api
      .accountingStatus()
      .then((status) => {
        if (!cancelled) setAccountingProvider(status.provider);
      })
      .catch(() => {
        if (!cancelled) setAccountingProvider(null);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    setDraft(
      editing
        ? draftFromRule(editing.rule)
        : draftForNewRule(seed),
    );
    setDirty(false);
    setRuleIdTouched(false);
    setError(null);
    setConflict(false);
  }, [editing, seed]);

  const isPristine = !dirty;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && isPristine) onCancel();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [isPristine, onCancel]);

  const validationErrors = useMemo(
    () => validate(draft, conditionCatalog),
    [conditionCatalog, draft],
  );
  const visibleValidationErrors = validationErrors.filter(
    (message) => message !== "Rule ID is required." || ruleIdTouched,
  );
  const draftValid = validationErrors.length === 0;

  useEffect(() => {
    onDraftChange(
      draftValid ? draftToRule(draft, conditionCatalog) : null,
      dirty,
    );
  }, [conditionCatalog, draft, dirty, draftValid, onDraftChange]);

  useEffect(() => {
    if (!seed || editing) {
      if (!seed) autoTestedSeedRef.current = null;
      return;
    }
    if (!conditionCatalog || !draftValid) return;
    if (autoTestedSeedRef.current === seed) return;
    autoTestedSeedRef.current = seed;
    onTestDraft(draftToRule(draft, conditionCatalog));
  }, [conditionCatalog, draft, draftValid, editing, onTestDraft, seed]);

  const update = (patch: Partial<Draft>) => {
    setDraft((d) => ({ ...d, ...patch }));
    setDirty(true);
  };

  const updateCondition = (index: number, patch: Partial<ConditionDraft>) => {
    setDraft((d) => ({
      ...d,
      conditions: d.conditions.map((c, i) =>
        i === index ? { ...c, ...patch } : c,
      ),
    }));
    setDirty(true);
  };

  const selectedCategory =
    categories.find(
      (category) => category.category_id === draft.pinned_category,
    ) ?? null;

  const createCategory = async (
    categoryDraft: CategoryCreateDraft,
  ): Promise<CategoryRecord> => {
    const sort = nextCategorySort(categories);
    const category: CategoryRecord = {
      category_id: categoryDraft.category_id,
      display_name: categoryDraft.display_name,
      description:
        categoryDraft.description.length > 0
          ? categoryDraft.description
          : categoryDraft.display_name,
      color: categoryDraft.color,
      sort,
      is_system: false,
      default_agent_dir: "",
      default_agent_context: "",
    };
    await api.upsertCategory({
      category,
      policy: normalizeWorkQueuePolicy({
        ...categoryDraft.policy,
        category_id: category.category_id,
      }),
      idempotency_key: crypto.randomUUID(),
      actor_id: null,
    });
    await refreshCategories();
    return category;
  };

  const save = async () => {
    if (!draftValid) return;
    setSaving(true);
    setError(null);
    setConflict(false);
    try {
      await api.upsertRule({
        rule: draftToRule(draft, conditionCatalog),
        expected_revision: editing ? editing.revision : null,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      onSaved();
    } catch (err) {
      if (isUnauthorized(err)) {
        onUnauthorized();
      } else if (isRevisionConflict(err)) {
        setConflict(true);
        onConflict();
      } else if (
        err instanceof ApiError &&
        err.code === "email_triage_category_unknown"
      ) {
        setError(
          "Pinned category isn't in the registry. Pick an existing category or create one in the Category field.",
        );
      } else {
        setError(errorMessage(err));
      }
    } finally {
      setSaving(false);
    }
  };

  const inputCls =
    "rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1.5 text-sm text-zinc-200 focus:border-sky-600 focus:outline-none";
  const inlineInputCls =
    "h-9 rounded-md border border-zinc-700 bg-zinc-900 px-2 text-sm text-zinc-200 focus:border-sky-600 focus:outline-none";

  return (
    <div className="rounded-lg border border-zinc-800 bg-zinc-900/60 p-4">
      <div className="mb-3 flex items-center justify-between">
        <h3 className="text-sm font-semibold text-zinc-200">
          {editing ? "Edit rule" : "New rule"}
        </h3>
        <Button variant="ghost" size="sm" onClick={onCancel}>
          Cancel
        </Button>
      </div>

      {seed && !editing ? (
        <div className="mb-3 rounded-md border border-sky-900/60 bg-sky-950/20 px-3 py-2 text-xs text-sky-200">
          Started from the selected inbox message. Adjust the fields, then test
          the draft against recent mail.
        </div>
      ) : null}

      <div className="grid grid-cols-2 gap-3 md:grid-cols-6">
        <label className="flex flex-col gap-1 text-xs text-zinc-400">
          Priority
          <input
            className={inputCls}
            type="number"
            value={draft.priority}
            onChange={(e) => update({ priority: e.target.value })}
          />
        </label>
        <label className="flex flex-col gap-1 text-xs text-zinc-400 md:col-span-2">
          Rule name
          <input
            className={inputCls}
            value={draft.rule_id}
            disabled={editing !== null}
            onChange={(e) => {
              setRuleIdTouched(true);
              update({ rule_id: e.target.value });
            }}
            placeholder="e.g. billing-follow-up"
          />
        </label>
      </div>

      <div className="mt-4">
        <div className="mb-2 flex flex-wrap items-center justify-between gap-2">
          <div className="flex flex-wrap items-center gap-2 text-xs text-zinc-400">
            <span className="font-semibold uppercase tracking-wide">
              Conditions
            </span>
            <span>Match</span>
            <select
              className="h-8 rounded-md border border-zinc-700 bg-zinc-900 px-2 text-xs text-zinc-200 focus:border-sky-600 focus:outline-none"
              value={draft.match_mode}
              onChange={(e) =>
                update({ match_mode: e.target.value as EmailTriageMatchMode })
              }
            >
              {MATCH_MODES.map((m) => (
                <option key={m} value={m}>
                  {m}
                </option>
              ))}
            </select>
            <span>of these</span>
          </div>
          <Button
            variant="secondary"
            size="sm"
            onClick={() => {
              setDraft((d) => ({
                ...d,
                conditions: [...d.conditions, emptyCondition()],
              }));
              setDirty(true);
            }}
          >
            + Add condition
          </Button>
        </div>
        <div className="flex flex-col gap-2">
          {draft.conditions.map((c, i) => {
              const item = catalogItemFor(conditionCatalog, c.condition_id);
              const aliasExpansion =
                item?.value_kind === "empty" ? item.expansion : null;
              const isAlias = aliasExpansion !== null;
              const showValue =
                item != null &&
                opRequiresValue(c.op) &&
                item.value_kind !== "bool" &&
                item.value_kind !== "empty";
              const showNoValue =
                item != null &&
                !isAlias &&
                !showValue &&
                item.value_kind !== "header";
              return (
                <div
                  key={i}
                  className="rounded-md border border-zinc-800 bg-zinc-950/60 p-2"
                >
                  <div className="flex flex-wrap items-center gap-2">
                    <label className="min-w-56 flex-1">
                      <span className="sr-only">Condition field</span>
                      <ConditionPicker
                        catalog={conditionCatalog}
                        value={c.condition_id}
                        inputCls={inlineInputCls}
                        onSelect={(nextId) => {
                          const nextItem = catalogItemFor(
                            conditionCatalog,
                            nextId,
                          );
                          const firstOp =
                            nextItem?.supported_ops[0] ?? DEFAULT_CONDITION_OP;
                          const nextValue = nextItem
                            ? defaultValueFor(nextItem)
                            : { value: "", headerName: "" };
                          updateCondition(i, {
                            condition_id: nextId,
                            op: firstOp,
                            value: nextValue.value,
                            header_name: nextValue.headerName,
                          });
                        }}
                      />
                    </label>
                    {item?.value_kind === "header" ? (
                      <label className="min-w-40 flex-1">
                        <span className="sr-only">Header name</span>
                        <input
                          className={`${inlineInputCls} w-full`}
                          value={c.header_name}
                          onChange={(e) =>
                            updateCondition(i, { header_name: e.target.value })
                          }
                          placeholder="X-Some-Header"
                        />
                      </label>
                    ) : null}
                    <label className="w-36">
                      <span className="sr-only">Operator</span>
                      <select
                        className={`${inlineInputCls} w-full`}
                        value={c.op}
                        disabled={!item}
                        onChange={(e) =>
                          updateCondition(i, {
                            op: e.target.value as EmailTriageConditionOperator,
                          })
                        }
                      >
                        {(item?.supported_ops ?? [DEFAULT_CONDITION_OP]).map(
                          (op) => (
                            <option key={op} value={op}>
                              {opLabel(op)}
                            </option>
                          ),
                        )}
                      </select>
                    </label>
                    {showValue ? (
                      <label className="min-w-40 flex-1">
                        <span className="sr-only">Value</span>
                        <input
                          className={`${inlineInputCls} w-full`}
                          value={c.value}
                          onChange={(e) =>
                            updateCondition(i, { value: e.target.value })
                          }
                        />
                      </label>
                    ) : null}
                    {showNoValue ? (
                      <span className="h-9 min-w-40 flex-1 px-2 py-2 text-sm text-zinc-500">
                        no value needed
                      </span>
                    ) : null}
                    <Button
                      variant="ghost"
                      size="sm"
                      className="h-9 w-9 px-0 text-zinc-500 hover:text-red-300"
                      aria-label="Remove condition"
                      title="Remove condition"
                      onClick={() => {
                        setDraft((d) => ({
                          ...d,
                          conditions: d.conditions.filter((_, j) => j !== i),
                        }));
                        setDirty(true);
                      }}
                    >
                      ×
                    </Button>
                  </div>
                  {isAlias ? (
                    <details className="mt-2 rounded border border-zinc-800 bg-zinc-900/60 px-3 py-2 text-xs text-zinc-400">
                      <summary className="cursor-pointer font-medium text-zinc-300">
                        How this quick pick works
                      </summary>
                      <div className="mt-2">
                        Matches {matchModeLabel(aliasExpansion.match_mode)} of
                        these facts:
                      </div>
                      <div className="mt-2 flex flex-col gap-1.5">
                        {aliasExpansion.conditions.map((condition) => (
                          <div
                            key={`${condition.condition_id}-${condition.op}-${condition.label}`}
                            className="rounded bg-zinc-800/80 px-2 py-1.5"
                          >
                            <div className="text-zinc-200">
                              {condition.label}
                            </div>
                            <div className="mt-0.5 text-[11px] text-zinc-500">
                              Source:{" "}
                              {conditionSourceLabel(
                                condition.condition_id,
                                conditionCatalog,
                                accountingProvider,
                              )}
                            </div>
                          </div>
                        ))}
                      </div>
                    </details>
                  ) : null}
                  {!isAlias ? (
                    <div className="mt-2 text-xs text-zinc-500">
                      {conditionSummary(c, item)}
                    </div>
                  ) : null}
                </div>
              );
            })}
        </div>
      </div>

      <div className="mt-4 border-t border-zinc-800 pt-4">
        <div className="mb-2 text-xs font-semibold uppercase tracking-wide text-zinc-400">
          Route matching mail to
        </div>
        <div className="max-w-xl">
          <CategoryPicker
            categories={categories}
            value={draft.pinned_category}
            inputCls={inputCls}
            onSelect={(categoryId) => update({ pinned_category: categoryId })}
            onCreate={createCategory}
            packetKindCatalog={packetKindCatalog}
            aiTriageEnabled={aiTriageEnabled}
          />
        </div>
        {selectedCategory ? (
          <div className="mt-2 max-w-xl text-xs text-zinc-500">
            Uses {categoryLabel(selectedCategory)}&apos;s existing work-item
            settings. This rule will not change them.
          </div>
        ) : null}
      </div>

      {dirty && visibleValidationErrors.length > 0 ? (
        <div className="mt-3 rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-2">
          <ul className="list-inside list-disc text-xs text-amber-300">
            {visibleValidationErrors.map((e, i) => (
              <li key={i}>{e}</li>
            ))}
          </ul>
        </div>
      ) : null}

      {conflict ? (
        <div className="mt-3 rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-2 text-xs text-amber-300">
          Rule changed elsewhere — reload. The list has been refetched; reopen
          the rule to edit the latest revision.
        </div>
      ) : null}
      {error ? (
        <div className="mt-3 rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-xs text-red-300">
          Save failed: {error}
        </div>
      ) : null}

      <div className="mt-4 flex items-center gap-3">
        <Button
          variant="primary"
          size="md"
          busy={saving}
          disabled={!draftValid}
          onClick={save}
        >
          {saving ? "Saving…" : editing ? "Save changes" : "Create rule"}
        </Button>
        <Button
          variant="secondary"
          size="md"
          disabled={!draftValid}
          className="border-sky-800/70 text-sky-400 hover:bg-sky-950/40"
          title="Run this unsaved rule against recent inbox messages without saving the rule or changing message categories."
          onClick={() => onTestDraft(draftToRule(draft, conditionCatalog))}
        >
          Dry run
        </Button>
        {previewSummary ? (
          <span
            className={
              previewSummary.loading
                ? "text-xs text-zinc-500"
                : "text-xs text-zinc-400"
            }
          >
            {previewSummary.loading
              ? "Checking recent mail..."
              : `Would match ${previewSummary.matched} of your last ${previewSummary.total} emails.`}
          </span>
        ) : null}
        {dirty ? (
          <span className="text-xs text-zinc-500">
            Unsaved changes are included in this test.
          </span>
        ) : null}
      </div>
    </div>
  );
}
