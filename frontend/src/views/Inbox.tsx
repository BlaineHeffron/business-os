import { useCallback, useEffect, useRef, useState } from "react";
import type { EmailTriageGmailCategory } from "../types/generated/EmailTriageGmailCategory";
import type { EmailTriageInboxOptionsResponse } from "../types/generated/EmailTriageInboxOptionsResponse";
import type { EmailTriageRule } from "../types/generated/EmailTriageRule";
import type { InboundMessageRecord } from "../types/generated/InboundMessageRecord";
import type { PacketProposalKindOutcome } from "../types/generated/PacketProposalKindOutcome";
import type { PacketProposalRun } from "../types/generated/PacketProposalRun";
import { ApiError, api, errorMessage, isUnauthorized } from "../lib/api";
import { useAppCommand } from "../lib/commands";
import { usePolling } from "../lib/usePolling";
import CategoryBadge from "../components/CategoryBadge";
import CrmContextLinks from "../components/CrmContextLinks";
import EmailBodyPreview, {
  detectEmailBodyFormat,
} from "../components/EmailBodyPreview";
import SectionHelpButton from "../components/SectionHelpButton";
import { Button, EmptyState, SkeletonRows } from "../components/ui";
import { categoryLabel, useCategories } from "../lib/categories";

const POLL_INTERVAL_MS = 30_000;
const SMART_DRAFT_POLL_MS = 3_000;
const ALL_VALUE = "__all";
const LEGACY_VALUE = "__legacy";
type CrmMatchFilter = "has_contact" | "no_match" | "has_deal";
type PendingSmartDraft = {
  sourceKey: string;
  runId: string;
};

const CRM_MATCH_FILTERS: { id: CrmMatchFilter; label: string }[] = [
  { id: "has_contact", label: "Has CRM contact" },
  { id: "no_match", label: "No CRM match" },
  { id: "has_deal", label: "Has associated deal" },
];

const GMAIL_TABS: { id: EmailTriageGmailCategory; label: string }[] = [
  { id: "primary", label: "Primary" },
  { id: "updates", label: "Updates" },
  { id: "social", label: "Social" },
  { id: "promotions", label: "Promotions" },
  { id: "forums", label: "Forums" },
];

function formatDate(ms: number | null): string {
  if (ms == null) return "—";
  return new Date(ms).toLocaleString();
}

// Gmail hides its system labels behind icons/tabs; only operator-meaningful
// labels (the ones rules filter on) render as chips in the list.
const SYSTEM_LABELS = new Set([
  "INBOX",
  "UNREAD",
  "SENT",
  "DRAFT",
  "SPAM",
  "TRASH",
  "STARRED",
  "IMPORTANT",
  "CHAT",
  "CATEGORY_PERSONAL",
  "CATEGORY_SOCIAL",
  "CATEGORY_PROMOTIONS",
  "CATEGORY_UPDATES",
  "CATEGORY_FORUMS",
]);

function userLabels(labels: string[]): string[] {
  return labels.filter((l) => !SYSTEM_LABELS.has(l));
}

function mailboxValue(sourceUserId: string | null): string {
  return sourceUserId ?? ALL_VALUE;
}

function mailboxDisplayName(
  sourceUserId: string | null,
  options: EmailTriageInboxOptionsResponse | null,
): string {
  if (sourceUserId == null) return "All source accounts";
  return (
    options?.mailboxes.find((mailbox) =>
      sourceUserId === LEGACY_VALUE
        ? mailbox.source_user_id === null
        : mailbox.source_user_id === sourceUserId,
    )?.display_name ?? sourceUserId
  );
}

function formatBytes(bytes: number | null): string {
  if (bytes === null) return "unknown size";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function visibleGmailTabs(options: EmailTriageInboxOptionsResponse | null) {
  const visible = new Set(
    options?.visible_gmail_categories ?? GMAIL_TABS.map((tab) => tab.id),
  );
  return GMAIL_TABS.filter((tab) => visible.has(tab.id));
}

function sanitizeActiveCategories(
  categories: EmailTriageGmailCategory[],
  options: EmailTriageInboxOptionsResponse,
): EmailTriageGmailCategory[] {
  const tabs = visibleGmailTabs(options);
  if (tabs.length <= 1) return [];
  const visible = new Set(tabs.map((tab) => tab.id));
  return categories.filter((category) => visible.has(category));
}

function sameGmailCategories(
  left: EmailTriageGmailCategory[],
  right: EmailTriageGmailCategory[],
): boolean {
  return (
    left.length === right.length &&
    left.every((category, idx) => category === right[idx])
  );
}

function resetListPosition(
  setSelectedId: (value: string | null) => void,
  setFocusIdx: (value: number) => void,
) {
  setSelectedId(null);
  setFocusIdx(0);
}

function smartDraftErrorMessage(err: unknown): string {
  if (err instanceof ApiError && err.status === 502) {
    return "Smart draft couldn't reach the AI service. Try again.";
  }
  return errorMessage(err);
}

function smartDraftNoDraftMessage(outcomes: PacketProposalKindOutcome[]): string {
  const details = outcomes
    .map((outcome) => {
      const reason = outcome.message?.trim() || outcome.reason_code;
      return reason ? `${outcome.packet_kind}: ${reason}` : outcome.packet_kind;
    })
    .filter((message) => message.trim().length > 0);
  const detail = details.slice(0, 3).join("; ");
  return detail
    ? `Smart draft finished with no drafts to review. ${detail}`
    : "Smart draft finished with no drafts to review.";
}

function smartDraftCompletionNotice(run: PacketProposalRun): {
  text: string;
  kind: "success" | "error";
} {
  if (run.status === "failed") {
    return {
      text: `Smart draft failed${run.error_code ? `: ${run.error_code}` : "."}`,
      kind: "error",
    };
  }
  const draftedCount = run.outcomes.filter(
    (outcome) => outcome.status === "drafted",
  ).length;
  return {
    text:
      draftedCount > 0
        ? "Drafts are ready in Queue."
        : smartDraftNoDraftMessage(run.outcomes),
    kind: "success",
  };
}

function activeFilterText(
  categories: EmailTriageGmailCategory[],
  dashboardCategories: string[],
  label: string | null,
  sourceUserId: string | null,
  crmMatch: CrmMatchFilter | null,
  crmDealStages: string[],
  crmDealPipelines: string[],
  search: string,
  options: EmailTriageInboxOptionsResponse | null,
  categoryNames: Map<string, string>,
  showSourceAccountFilter: boolean,
): string {
  const parts: string[] = [];
  if (search.trim()) parts.push(`"${search.trim()}"`);
  if (categories.length > 0) {
    parts.push(
      categories
        .map((category) => GMAIL_TABS.find((tab) => tab.id === category)?.label ?? category)
        .join(" + "),
    );
  }
  if (dashboardCategories.length > 0) {
    parts.push(
      dashboardCategories
        .map((categoryId) => categoryNames.get(categoryId) ?? categoryId)
        .join(" + "),
    );
  }
  if (label) parts.push(label);
  if (showSourceAccountFilter && sourceUserId) {
    parts.push(mailboxDisplayName(sourceUserId, options));
  }
  if (crmMatch) {
    parts.push(CRM_MATCH_FILTERS.find((filter) => filter.id === crmMatch)?.label ?? crmMatch);
  }
  if (crmDealStages.length > 0) parts.push(`Stage: ${crmDealStages.join(" + ")}`);
  if (crmDealPipelines.length > 0) {
    parts.push(`Pipeline: ${crmDealPipelines.join(" + ")}`);
  }
  return parts.length > 0 ? parts.join(" · ") : "All mail";
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

function ActiveChip({
  label,
  tone = "zinc",
  color,
  onClear,
}: {
  label: string;
  tone?: "sky" | "zinc";
  color?: string;
  onClear: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onClear}
      className={`inline-flex h-7 max-w-60 items-center gap-1.5 rounded-full border px-2.5 text-xs font-medium focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70 ${
        tone === "sky"
          ? "border-sky-700/70 bg-sky-950/40 text-sky-100 hover:bg-sky-900/50"
          : "border-zinc-700 bg-zinc-900 text-zinc-200 hover:border-zinc-500 hover:bg-zinc-800"
      }`}
      style={
        color
          ? {
              borderColor: color,
              boxShadow: `inset 0 0 0 1px ${color}33`,
            }
          : undefined
      }
      title={`Clear ${label}`}
    >
      {color ? (
        <span
          aria-hidden
          className="h-2 w-2 shrink-0 rounded-full"
          style={{ backgroundColor: color }}
        />
      ) : null}
      <span className="truncate">{label}</span>
      <span aria-hidden className="text-zinc-500">
        x
      </span>
    </button>
  );
}

/** "name <addr@host>" → "addr@host"; bare addresses pass through. */
function extractAddress(raw: string): string {
  const match = raw.match(/<([^>]+)>/);
  return (match ? match[1] : raw).trim().toLowerCase();
}

function slugify(raw: string): string {
  return raw
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48);
}

/** Seed a rule from a message, Gmail-filter style: sender or sender-domain. */
function ruleSeedFromMessage(
  message: InboundMessageRecord,
  scope: "sender" | "domain",
): EmailTriageRule | null {
  const address = extractAddress(message.from_addr ?? "");
  if (!address.includes("@")) return null;
  const domain = address.split("@")[1] ?? "";
  const value = scope === "sender" ? address : `@${domain}`;
  const idBase = scope === "sender" ? address : domain;
  return {
    rule_id: `from-${slugify(idBase)}`,
    conditions: [
      { field: "from", op: "contains", value, header_name: null },
    ],
    conditions_v2: [],
    match_mode: "all",
    priority: 100,
    enabled: true,
    pinned_category: message.resolved_category,
  };
}

export default function Inbox({
  onUnauthorized,
  helpTopicId,
  onOpenHelpTopic,
  onCreateRule,
  focusMessageId,
  onFocusMessageConsumed,
}: {
  onUnauthorized: () => void;
  helpTopicId?: string;
  onOpenHelpTopic: (topicId: string) => void;
  /** Hand a prefilled rule to the Rules view ("create rule from this email"). */
  onCreateRule: (seed: EmailTriageRule) => void;
  focusMessageId?: string | null;
  onFocusMessageConsumed?: () => void;
}) {
  const { categories: dashboardCategoryRecords } = useCategories();
  const [messages, setMessages] = useState<InboundMessageRecord[]>([]);
  const [options, setOptions] = useState<EmailTriageInboxOptionsResponse | null>(null);
  const [activeCategories, setActiveCategories] = useState<EmailTriageGmailCategory[]>([]);
  const [activeDashboardCategories, setActiveDashboardCategories] = useState<string[]>([]);
  const [activeLabel, setActiveLabel] = useState<string | null>(null);
  const [activeSourceUserId, setActiveSourceUserId] = useState<string | null>(null);
  const [activeCrmMatch, setActiveCrmMatch] = useState<CrmMatchFilter | null>(null);
  const [activeCrmDealStages, setActiveCrmDealStages] = useState<string[]>([]);
  const [activeCrmDealPipelines, setActiveCrmDealPipelines] = useState<string[]>([]);
  const [searchDraft, setSearchDraft] = useState("");
  const [activeSearch, setActiveSearch] = useState("");
  const [limit, setLimit] = useState(100);
  const [filtersReady, setFiltersReady] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ text: string; kind: "success" | "error" } | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [focusIdx, setFocusIdx] = useState(0);
  const [followUpBusyId, setFollowUpBusyId] = useState<string | null>(null);
  const [trashBusyId, setTrashBusyId] = useState<string | null>(null);
  const [smartDraftBusyId, setSmartDraftBusyId] = useState<string | null>(null);
  const [pendingSmartDraft, setPendingSmartDraft] = useState<PendingSmartDraft | null>(null);
  const optionsRef = useRef<EmailTriageInboxOptionsResponse | null>(null);
  const isFirstLoad = !loaded;

  const loadOptions = useCallback(async (applyDefaults: boolean) => {
    try {
      const res = await api.inboxOptions();
      optionsRef.current = res;
      setOptions(res);
      if (applyDefaults) {
        setActiveCategories(sanitizeActiveCategories(res.defaults.categories, res));
        setActiveLabel(res.defaults.label);
        setActiveSourceUserId(
          res.mailboxes.length > 1 ? res.defaults.source_user_id : null,
        );
        setLimit(res.defaults.limit);
      } else {
        setActiveCategories((current) => {
          const next = sanitizeActiveCategories(current, res);
          return sameGmailCategories(current, next) ? current : next;
        });
      }
      setFiltersReady(true);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else if (applyDefaults) {
        setError(errorMessage(err));
        setLoaded(true);
      }
    }
  }, [onUnauthorized]);

  const load = useCallback(async () => {
    if (!filtersReady) return;
    setRefreshing(true);
    try {
      const currentOptions = optionsRef.current;
      const categories = currentOptions
        ? sanitizeActiveCategories(activeCategories, currentOptions)
        : activeCategories;
      const res = await api.inbox({
        categories,
        dashboardCategories: activeDashboardCategories,
        label: activeLabel,
        sourceUserId: activeSourceUserId,
        crmMatch: activeCrmMatch,
        crmDealStages: activeCrmDealStages,
        crmDealPipelines: activeCrmDealPipelines,
        q: activeSearch,
        limit,
      });
      setMessages(res.messages);
      setError(null);
      void loadOptions(false);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    } finally {
      setRefreshing(false);
      setLoaded(true);
    }
  }, [
    activeCategories,
    activeDashboardCategories,
    activeLabel,
    activeCrmDealPipelines,
    activeCrmDealStages,
    activeCrmMatch,
    activeSearch,
    activeSourceUserId,
    filtersReady,
    limit,
    loadOptions,
    onUnauthorized,
  ]);

  useAppCommand("refresh", () => void load());

  useEffect(() => {
    void loadOptions(true);
  }, [loadOptions]);

  usePolling(load, { enabled: filtersReady, intervalMs: POLL_INTERVAL_MS });

  const selected = messages.find((m) => m.source_key === selectedId) ?? null;

  // Row refs for scrollIntoView on keyboard navigation.
  const rowRefs = useRef<Map<string, HTMLTableRowElement>>(new Map());

  // j/k + Enter/Esc keyboard navigation mirroring Queue's pattern.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey) return;
      const target = e.target as HTMLElement | null;
      if (
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
          setFocusIdx((i) => {
            const next = Math.min(i + 1, Math.max(messages.length - 1, 0));
            const msg = messages[next];
            if (msg) {
              rowRefs.current.get(msg.source_key)?.scrollIntoView({ block: "nearest" });
            }
            return next;
          });
          break;
        case "k":
          e.preventDefault();
          setFocusIdx((i) => {
            const next = Math.max(Math.min(i, messages.length - 1) - 1, 0);
            const msg = messages[next];
            if (msg) {
              rowRefs.current.get(msg.source_key)?.scrollIntoView({ block: "nearest" });
            }
            return next;
          });
          break;
        case "Enter": {
          const msg = messages[focusIdx];
          if (!msg) return;
          e.preventDefault();
          setSelectedId((prev) => (prev === msg.source_key ? null : msg.source_key));
          break;
        }
        case "Escape":
          e.preventDefault();
          setSelectedId(null);
          break;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  const focusedId = messages[focusIdx]?.source_key ?? null;

  useEffect(() => {
    if (!focusMessageId || !loaded) return;
    const idx = messages.findIndex((message) => message.source_key === focusMessageId);
    if (idx === -1) {
      onFocusMessageConsumed?.();
      return;
    }
    setFocusIdx(idx);
    setSelectedId(focusMessageId);
    requestAnimationFrame(() => {
      rowRefs.current.get(focusMessageId)?.scrollIntoView({ block: "center" });
    });
    onFocusMessageConsumed?.();
  }, [focusMessageId, loaded, messages, onFocusMessageConsumed]);

  const categoryNames = new Map(
    dashboardCategoryRecords.map((category) => [
      category.category_id,
      categoryLabel(category),
    ]),
  );
  const dashboardCategoryCounts = new Map(
    options?.dashboard_categories.map((category) => [
      category.category_id,
      category.count,
    ]) ?? [],
  );
  const dashboardCategoryIds = [
    ...dashboardCategoryRecords.map((category) => category.category_id),
    ...(options?.dashboard_categories
      .map((category) => category.category_id)
      .filter(
        (categoryId) =>
          !dashboardCategoryRecords.some(
            (category) => category.category_id === categoryId,
          ),
      ) ?? []),
  ];
  const sourceAccountOptions = options?.mailboxes ?? [];
  const showSourceAccountFilter = sourceAccountOptions.length > 1;
  const gmailTabs = visibleGmailTabs(options);
  const showGmailTabs = gmailTabs.length > 1;
  const visibleActiveCategories = showGmailTabs ? activeCategories : [];
  const filterText = activeFilterText(
    visibleActiveCategories,
    activeDashboardCategories,
    activeLabel,
    activeSourceUserId,
    activeCrmMatch,
    activeCrmDealStages,
    activeCrmDealPipelines,
    activeSearch,
    options,
    categoryNames,
    showSourceAccountFilter,
  );
  const allFiltersClear =
    visibleActiveCategories.length === 0 &&
    activeDashboardCategories.length === 0 &&
    activeLabel === null &&
    (!showSourceAccountFilter || activeSourceUserId === null) &&
    activeCrmMatch === null &&
    activeCrmDealStages.length === 0 &&
    activeCrmDealPipelines.length === 0 &&
    activeSearch.trim() === "";
  const hasActiveSummaryFilters =
    visibleActiveCategories.length > 0 ||
    activeLabel !== null ||
    (showSourceAccountFilter && activeSourceUserId !== null) ||
    activeCrmMatch !== null ||
    activeCrmDealStages.length > 0 ||
    activeCrmDealPipelines.length > 0 ||
    activeSearch.trim() !== "";

  const clearAllFilters = () => {
    resetListPosition(setSelectedId, setFocusIdx);
    setActiveCategories([]);
    setActiveDashboardCategories([]);
    setActiveLabel(null);
    setActiveSourceUserId(null);
    setActiveCrmMatch(null);
    setActiveCrmDealStages([]);
    setActiveCrmDealPipelines([]);
    setSearchDraft("");
    setActiveSearch("");
  };

  const clearCrmDealStage = (stage: string) => {
    resetListPosition(setSelectedId, setFocusIdx);
    setActiveCrmDealStages((current) => current.filter((value) => value !== stage));
  };

  const clearCrmDealPipeline = (pipeline: string) => {
    resetListPosition(setSelectedId, setFocusIdx);
    setActiveCrmDealPipelines((current) =>
      current.filter((value) => value !== pipeline),
    );
  };

  const clearDashboardCategory = (categoryId: string) => {
    resetListPosition(setSelectedId, setFocusIdx);
    setActiveDashboardCategories((current) =>
      current.filter((value) => value !== categoryId),
    );
  };

  const toggleCategory = (category: EmailTriageGmailCategory) => {
    resetListPosition(setSelectedId, setFocusIdx);
    setActiveCategories((current) =>
      current.includes(category)
        ? current.filter((value) => value !== category)
        : [...current, category],
    );
  };

  const submitSearch = () => {
    resetListPosition(setSelectedId, setFocusIdx);
    setActiveSearch(searchDraft.trim());
  };

  const clearSearch = () => {
    resetListPosition(setSelectedId, setFocusIdx);
    setSearchDraft("");
    setActiveSearch("");
  };

  const addFollowUp = async (messageId: string) => {
    if (followUpBusyId) return;
    setFollowUpBusyId(messageId);
    setNotice(null);
    setError(null);
    try {
      await api.addInboxFollowUp(messageId, {
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNotice({
        text: "Follow-up added. Review it from Queue.",
        kind: "success",
      });
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else {
        setNotice({
          text: `Couldn't add follow-up: ${errorMessage(err)}`,
          kind: "error",
        });
      }
    } finally {
      setFollowUpBusyId(null);
    }
  };

  const trashEmail = async (message: InboundMessageRecord) => {
    if (trashBusyId) return;
    if (
      !window.confirm(
        `Move “${message.subject ?? "(no subject)"}” to Gmail Trash? This also dismisses its Queue item.`,
      )
    ) {
      return;
    }
    setTrashBusyId(message.source_key);
    setNotice(null);
    setError(null);
    try {
      await api.trashInboxEmail(message.source_key, {
        expected_revision: null,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setMessages((current) =>
        current.filter((candidate) => candidate.source_key !== message.source_key),
      );
      setSelectedId((current) =>
        current === message.source_key ? null : current,
      );
      setNotice({
        text: "Trash requested. Gmail keeps the message recoverable until Trash is emptied.",
        kind: "success",
      });
      void loadOptions(false);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else {
        setNotice({
          text: `Couldn't move email to Trash: ${errorMessage(err)}`,
          kind: "error",
        });
      }
    } finally {
      setTrashBusyId(null);
    }
  };

  const startSmartDraft = async (message: InboundMessageRecord) => {
    if (smartDraftBusyId) return;
    setSmartDraftBusyId(message.source_key);
    setNotice(null);
    setError(null);
    try {
      const sourceState = await api.smartDraftSourceState({
        source_kind: "email",
        source_ref: message.source_key,
        run_id: null,
      });
      const response = await api.smartDraft({
        source_kind: "email",
        source_ref: message.source_key,
        idempotency_key: crypto.randomUUID(),
        expected_revision: sourceState.expected_revision,
        actor_id: null,
      });
      if (response.run.status === "running") {
        setPendingSmartDraft({
          sourceKey: message.source_key,
          runId: response.run.run_id,
        });
        setNotice({
          text: "Smart draft is still working. You can leave Inbox; it will keep running.",
          kind: "success",
        });
        return;
      }
      setPendingSmartDraft(null);
      setSmartDraftBusyId(null);
      setNotice(smartDraftCompletionNotice(response.run));
    } catch (err) {
      setPendingSmartDraft(null);
      setSmartDraftBusyId(null);
      if (isUnauthorized(err)) {
        onUnauthorized();
      } else {
        setNotice({
          text: `Couldn't prepare drafts: ${smartDraftErrorMessage(err)}`,
          kind: "error",
        });
      }
    }
  };

  useEffect(() => {
    if (!pendingSmartDraft) return;
    let cancelled = false;
    setSmartDraftBusyId(pendingSmartDraft.sourceKey);

    const poll = async () => {
      try {
        const response = await api.smartDraftSourceState({
          source_kind: "email",
          source_ref: pendingSmartDraft.sourceKey,
          run_id: pendingSmartDraft.runId,
        });
        if (cancelled || !response.run || response.run.status === "running") return;
        setPendingSmartDraft(null);
        setSmartDraftBusyId(null);
        setNotice(smartDraftCompletionNotice(response.run));
      } catch (err) {
        if (cancelled) return;
        if (isUnauthorized(err)) {
          setSmartDraftBusyId(null);
          onUnauthorized();
          return;
        }
        setPendingSmartDraft(null);
        setSmartDraftBusyId(null);
        setNotice({
          text: `Couldn't prepare drafts: ${smartDraftErrorMessage(err)}`,
          kind: "error",
        });
      }
    };

    void poll();
    const interval = window.setInterval(() => void poll(), SMART_DRAFT_POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(interval);
    };
  }, [onUnauthorized, pendingSmartDraft]);

  return (
    <div>
      <div className="mb-3 flex items-center justify-between">
        <div className="flex items-center gap-2">
          <h2 className="text-lg font-semibold text-zinc-100">Inbox</h2>
          <SectionHelpButton
            topicId={helpTopicId}
            onOpenHelp={onOpenHelpTopic}
            label="Open help for Inbox"
          />
        </div>
        <div className="flex items-center gap-3">
          <span
            className="hidden text-xs text-zinc-500 sm:inline"
            title="Keyboard: j/k move focus, Enter opens detail, Esc closes"
          >
            j/k · ⏎ open · esc close
          </span>
          <span className="text-xs text-zinc-400">
            {messages.length} message{messages.length === 1 ? "" : "s"}
            {messages.length >= limit ? ` (latest ${limit})` : ""} ·
            polls every 30s
          </span>
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

      <form
        className="surface-card surface-flat surface-body-sky mb-3 flex min-w-0 items-center gap-2 rounded-lg border border-zinc-800 bg-zinc-950 px-2 py-2 focus-within:border-sky-700/70 focus-within:ring-1 focus-within:ring-sky-700/50"
        onSubmit={(e) => {
          e.preventDefault();
          submitSearch();
        }}
        role="search"
        aria-label="Search inbox"
      >
        <span aria-hidden className="px-1 text-zinc-500">
          ⌕
        </span>
        <input
          className="h-9 min-w-0 flex-1 bg-transparent text-sm text-zinc-100 placeholder:text-zinc-500 focus:outline-none"
          value={searchDraft}
          onChange={(e) => setSearchDraft(e.target.value)}
          placeholder='Search mail'
          aria-label="Search mail"
        />
        {activeSearch ? (
          <button
            type="button"
            onClick={clearSearch}
            className="h-8 rounded-md px-2 text-xs font-medium text-zinc-400 hover:bg-zinc-900 hover:text-zinc-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70"
          >
            Clear
          </button>
        ) : null}
        <Button variant="secondary" size="sm" type="submit">
          Search
        </Button>
      </form>

      <div className="surface-card surface-flat surface-body-sky mb-3 border-y border-zinc-800 py-2">
        {showGmailTabs ? (
          <div
            role="tablist"
            aria-label="Gmail category"
            className="flex min-w-0 flex-wrap items-center gap-x-1 border-b border-zinc-800"
          >
            <button
              type="button"
              role="tab"
              aria-selected={allFiltersClear}
              onClick={clearAllFilters}
              className={`-mb-px border-b-2 px-3 pb-2 pt-1 text-sm font-medium transition focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70 ${
                allFiltersClear
                  ? "border-sky-500 text-zinc-100"
                  : "border-transparent text-zinc-400 hover:text-zinc-200"
              }`}
              title="Show all mail"
            >
              All
            </button>
            {gmailTabs.map((tab) => {
              const count =
                options?.categories.find((category) => category.category === tab.id)
                  ?.count ?? 0;
              const active = activeCategories.includes(tab.id);
              return (
                <button
                  key={tab.id}
                  type="button"
                  role="tab"
                  aria-selected={active}
                  onClick={() => toggleCategory(tab.id)}
                  className={`-mb-px inline-flex items-center gap-1.5 border-b-2 px-3 pb-2 pt-1 text-sm font-medium transition focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70 ${
                    active
                      ? "border-sky-500 text-zinc-100"
                      : "border-transparent text-zinc-400 hover:text-zinc-200"
                  }`}
                  title={`${tab.label} Gmail tab`}
                >
                  {tab.label}
                  <span
                    className={`rounded-full px-1.5 text-[11px] tabular-nums ${
                      active
                        ? "bg-sky-500/20 text-sky-200"
                        : "bg-zinc-800 text-zinc-400"
                    }`}
                  >
                    {count}
                  </span>
                </button>
              );
            })}
          </div>
        ) : null}

        <div
          className={`${showGmailTabs ? "mt-2" : ""} flex flex-col gap-2 lg:flex-row lg:items-start lg:justify-between`}
        >
          <div className="flex min-w-0 flex-1 flex-wrap items-center gap-1.5">
            {dashboardCategoryIds.length > 0 ? (
              <label className="flex items-center gap-2 text-xs text-zinc-400">
                <span className="font-medium text-zinc-500">Category</span>
                <select
                  className="h-8 max-w-64 rounded-md border border-zinc-700 bg-zinc-950 px-2 text-xs text-zinc-200"
                  value={ALL_VALUE}
                  onChange={(e) => {
                    const categoryId = e.target.value;
                    if (categoryId === ALL_VALUE) {
                      resetListPosition(setSelectedId, setFocusIdx);
                      setActiveDashboardCategories([]);
                      return;
                    }
                    if (!activeDashboardCategories.includes(categoryId)) {
                      resetListPosition(setSelectedId, setFocusIdx);
                      setActiveDashboardCategories((current) => [
                        ...current,
                        categoryId,
                      ]);
                    }
                  }}
                >
                  <option value={ALL_VALUE}>
                    {activeDashboardCategories.length > 0
                      ? "Add category..."
                      : "All categories"}
                  </option>
                  {dashboardCategoryIds.map((categoryId) => {
                    const name = categoryNames.get(categoryId) ?? categoryId;
                    const count = dashboardCategoryCounts.get(categoryId) ?? 0;
                    return (
                      <option
                        key={categoryId}
                        value={categoryId}
                        disabled={activeDashboardCategories.includes(categoryId)}
                      >
                        {name} ({count})
                      </option>
                    );
                  })}
                </select>
              </label>
            ) : null}
            {activeDashboardCategories.map((categoryId) => {
              const record =
                dashboardCategoryRecords.find(
                  (category) => category.category_id === categoryId,
                ) ?? null;
              return (
                <ActiveChip
                  key={categoryId}
                  label={categoryNames.get(categoryId) ?? categoryId}
                  color={record?.color ?? "#a1a1aa"}
                  onClear={() => clearDashboardCategory(categoryId)}
                />
              );
            })}
          </div>

          <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
            <label className="flex items-center gap-2 text-xs text-zinc-400">
              <span className="font-medium text-zinc-500">Gmail label</span>
              <select
                className="h-8 rounded-md border border-zinc-700 bg-zinc-950 px-2 text-xs text-zinc-200"
                value={activeLabel ?? ALL_VALUE}
                onChange={(e) => {
                  resetListPosition(setSelectedId, setFocusIdx);
                  setActiveLabel(e.target.value === ALL_VALUE ? null : e.target.value);
                }}
              >
                <option value={ALL_VALUE}>All labels</option>
                {options?.labels.map((label) => (
                  <option key={label.label} value={label.label}>
                    {label.label} ({label.count})
                  </option>
                ))}
              </select>
            </label>
            {showSourceAccountFilter ? (
              <label className="flex items-center gap-2 text-xs text-zinc-400">
                <span className="font-medium text-zinc-500">Source account</span>
                <select
                  className="h-8 rounded-md border border-zinc-700 bg-zinc-950 px-2 text-xs text-zinc-200"
                  value={mailboxValue(activeSourceUserId)}
                  onChange={(e) => {
                    resetListPosition(setSelectedId, setFocusIdx);
                    setActiveSourceUserId(
                      e.target.value === ALL_VALUE ? null : e.target.value,
                    );
                  }}
                >
                  <option value={ALL_VALUE}>All source accounts</option>
                  {sourceAccountOptions.map((mailbox) => (
                    <option
                      key={mailbox.source_user_id ?? "legacy"}
                      value={mailbox.source_user_id ?? LEGACY_VALUE}
                    >
                      {mailbox.display_name} ({mailbox.count})
                    </option>
                  ))}
                </select>
              </label>
            ) : null}
          </div>
        </div>

        <div className="mt-2 flex flex-col gap-2 sm:flex-row sm:flex-wrap sm:items-center">
          <label className="flex items-center gap-2 text-xs text-zinc-400">
            <span className="font-medium text-zinc-500">CRM</span>
            <select
              className="h-8 rounded-md border border-zinc-700 bg-zinc-950 px-2 text-xs text-zinc-200"
              value={activeCrmMatch ?? ALL_VALUE}
              onChange={(e) => {
                resetListPosition(setSelectedId, setFocusIdx);
                setActiveCrmMatch(
                  e.target.value === ALL_VALUE
                    ? null
                    : (e.target.value as CrmMatchFilter),
                );
              }}
            >
              <option value={ALL_VALUE}>Any CRM match</option>
              {CRM_MATCH_FILTERS.map((filter) => (
                <option key={filter.id} value={filter.id}>
                  {filter.label}
                </option>
              ))}
            </select>
          </label>
          <label className="flex items-center gap-2 text-xs text-zinc-400">
            <span className="font-medium text-zinc-500">Deal stage</span>
            <select
              className="h-8 rounded-md border border-zinc-700 bg-zinc-950 px-2 text-xs text-zinc-200"
              value={ALL_VALUE}
              onChange={(e) => {
                const stage = e.target.value;
                if (stage === ALL_VALUE) {
                  resetListPosition(setSelectedId, setFocusIdx);
                  setActiveCrmDealStages([]);
                  return;
                }
                if (!activeCrmDealStages.includes(stage)) {
                  resetListPosition(setSelectedId, setFocusIdx);
                  setActiveCrmDealStages((current) => [...current, stage]);
                }
              }}
            >
              <option value={ALL_VALUE}>
                {activeCrmDealStages.length > 0 ? "Add stage..." : "All stages"}
              </option>
              {options?.crm_deal_stages.map((stage) => (
                <option
                  key={stage.value}
                  value={stage.value}
                  disabled={activeCrmDealStages.includes(stage.value)}
                >
                  {stage.value} ({stage.count})
                </option>
              ))}
            </select>
          </label>
          <label className="flex items-center gap-2 text-xs text-zinc-400">
            <span className="font-medium text-zinc-500">Pipeline</span>
            <select
              className="h-8 rounded-md border border-zinc-700 bg-zinc-950 px-2 text-xs text-zinc-200"
              value={ALL_VALUE}
              onChange={(e) => {
                const pipeline = e.target.value;
                if (pipeline === ALL_VALUE) {
                  resetListPosition(setSelectedId, setFocusIdx);
                  setActiveCrmDealPipelines([]);
                  return;
                }
                if (!activeCrmDealPipelines.includes(pipeline)) {
                  resetListPosition(setSelectedId, setFocusIdx);
                  setActiveCrmDealPipelines((current) => [...current, pipeline]);
                }
              }}
            >
              <option value={ALL_VALUE}>
                {activeCrmDealPipelines.length > 0
                  ? "Add pipeline..."
                  : "All pipelines"}
              </option>
              {options?.crm_deal_pipelines.map((pipeline) => (
                <option
                  key={pipeline.value}
                  value={pipeline.value}
                  disabled={activeCrmDealPipelines.includes(pipeline.value)}
                >
                  {pipeline.value} ({pipeline.count})
                </option>
              ))}
            </select>
          </label>
        </div>

        {hasActiveSummaryFilters ? (
          <div className="mt-2 flex flex-wrap items-center gap-1.5 rounded-md border border-zinc-800 bg-zinc-900/40 px-2 py-1.5">
            <span className="mr-0.5 text-[10px] font-semibold uppercase tracking-wide text-zinc-500">
              Active
            </span>
            {visibleActiveCategories.map((category) => (
              <ActiveChip
                key={`gmail-${category}`}
                label={
                  GMAIL_TABS.find((tab) => tab.id === category)?.label ?? category
                }
                tone="sky"
                onClear={() => toggleCategory(category)}
              />
            ))}
            {activeLabel ? (
              <ActiveChip
                label={activeLabel}
                onClear={() => {
                  resetListPosition(setSelectedId, setFocusIdx);
                  setActiveLabel(null);
                }}
              />
            ) : null}
            {showSourceAccountFilter && activeSourceUserId ? (
              <ActiveChip
                label={mailboxDisplayName(activeSourceUserId, options)}
                onClear={() => {
                  resetListPosition(setSelectedId, setFocusIdx);
                  setActiveSourceUserId(null);
                }}
              />
            ) : null}
            {activeCrmMatch ? (
              <ActiveChip
                label={
                  CRM_MATCH_FILTERS.find((filter) => filter.id === activeCrmMatch)
                    ?.label ?? activeCrmMatch
                }
                onClear={() => {
                  resetListPosition(setSelectedId, setFocusIdx);
                  setActiveCrmMatch(null);
                }}
              />
            ) : null}
            {activeCrmDealStages.map((stage) => (
              <ActiveChip
                key={`crm-stage-${stage}`}
                label={`Stage: ${stage}`}
                onClear={() => clearCrmDealStage(stage)}
              />
            ))}
            {activeCrmDealPipelines.map((pipeline) => (
              <ActiveChip
                key={`crm-pipeline-${pipeline}`}
                label={`Pipeline: ${pipeline}`}
                onClear={() => clearCrmDealPipeline(pipeline)}
              />
            ))}
            {activeSearch ? (
              <ActiveChip label={`Search: ${activeSearch}`} onClear={clearSearch} />
            ) : null}
            <button
              type="button"
              onClick={clearAllFilters}
              className="ml-auto text-xs font-medium text-zinc-400 hover:text-zinc-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70"
            >
              Clear all
            </button>
          </div>
        ) : null}
      </div>

      {error ? (
        <div className="mb-3 rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300">
          Failed to load inbox: {error}
        </div>
      ) : null}

      {notice ? (
        <div
          className={`mb-3 rounded-md border px-3 py-2 text-sm ${
            notice.kind === "success"
              ? "border-emerald-900/60 bg-emerald-950/30 text-emerald-300"
              : "border-red-900/60 bg-red-950/40 text-red-300"
          }`}
        >
          {notice.text}
        </div>
      ) : null}

      {loaded && messages.length === 0 && !error ? (
        <EmptyState
          title={
            filterText === "All mail"
              ? "No messages yet."
              : "No messages match these filters."
          }
        >
          {filterText === "All mail"
            ? "Connected Gmail messages are sorted by your rules and shown here. Connect Gmail and give it a minute to sync."
            : `${filterText} has no matching messages.`}
        </EmptyState>
      ) : null}

      {!loaded && isFirstLoad ? (
        <div className="surface-card surface-flat surface-body-sky overflow-x-auto rounded-lg border border-zinc-800">
          <table className="w-full text-left text-sm">
            <thead className="surface-head-sky bg-zinc-900 text-xs uppercase tracking-wide text-zinc-400">
              <tr>
                <th className="px-3 py-2">Category</th>
                <th className="px-3 py-2">From</th>
                <th className="px-3 py-2">Subject</th>
                <th className="px-3 py-2">Date</th>
                <th className="px-3 py-2">Rule</th>
                <th className="px-3 py-2"></th>
              </tr>
            </thead>
            <tbody className="surface-row-divide divide-y divide-zinc-800/80">
              <SkeletonRows rows={5} cols={6} />
            </tbody>
          </table>
        </div>
      ) : null}

      {messages.length > 0 ? (
        <div className="flex flex-col gap-4 lg:flex-row">
          <div className="surface-card surface-flat surface-body-sky min-w-0 flex-1 overflow-x-auto rounded-lg border border-zinc-800">
            <table className="w-full text-left text-sm">
              <thead className="surface-head-sky bg-zinc-900 text-xs uppercase tracking-wide text-zinc-400">
                <tr>
                  <th className="px-3 py-2">Category</th>
                  <th className="px-3 py-2">From</th>
                  <th className="px-3 py-2">Subject</th>
                  <th className="px-3 py-2">Date</th>
                  <th className="px-3 py-2">Rule</th>
                  <th className="px-3 py-2"></th>
                </tr>
              </thead>
              <tbody className="surface-row-divide divide-y divide-zinc-800/80">
                {messages.map((m, idx) => {
                  const isFocused = m.source_key === focusedId;
                  const isSelected = m.source_key === selectedId;
                  return (
                    <tr
                      key={m.source_key}
                      ref={(el) => {
                        if (el) rowRefs.current.set(m.source_key, el);
                        else rowRefs.current.delete(m.source_key);
                      }}
                      onClick={() => {
                        setFocusIdx(idx);
                        setSelectedId(m.source_key);
                      }}
                      className={`cursor-pointer hover:bg-zinc-900/80 ${
                        isSelected ? "bg-zinc-900" : ""
                      } ${isFocused ? "bg-zinc-900/60" : ""}`}
                    >
                      <td className="relative px-3 py-2">
                        {isFocused ? (
                          <span
                            aria-hidden
                            className="absolute inset-y-0 left-0 w-0.5 rounded-l bg-sky-500"
                          />
                        ) : null}
                        <CategoryBadge category={m.resolved_category} />
                      </td>
                      <td className="max-w-48 truncate px-3 py-2 text-zinc-300">
                        {m.from_addr ?? "—"}
                      </td>
                      <td className="max-w-72 px-3 py-2 text-zinc-200">
                        <span className="flex items-center gap-1.5">
                          <LabelChips labels={userLabels(m.labels)} />
                          <span className="min-w-0 truncate">
                            {m.subject ?? "(no subject)"}
                          </span>
                        </span>
                      </td>
                      <td className="whitespace-nowrap px-3 py-2 text-zinc-400">
                        {formatDate(m.internal_date_ms)}
                      </td>
                      <td className="px-3 py-2 font-mono text-xs text-zinc-400">
                        {m.matched_rule_id ?? "—"}
                      </td>
                      <td className="whitespace-nowrap px-3 py-2 text-right">
                        <div className="flex justify-end gap-2">
                          <Button
                            variant="primary"
                            size="sm"
                            busy={smartDraftBusyId === m.source_key}
                            disabled={followUpBusyId === m.source_key}
                            onClick={(e) => {
                              e.stopPropagation();
                              void startSmartDraft(m);
                            }}
                            title="Prepare drafts from this email for review"
                          >
                            {smartDraftBusyId === m.source_key
                              ? "Drafting..."
                              : "Smart draft"}
                          </Button>
                          <Button
                            variant="secondary"
                            size="sm"
                            busy={followUpBusyId === m.source_key}
                            disabled={smartDraftBusyId === m.source_key}
                            onClick={(e) => {
                              e.stopPropagation();
                              void addFollowUp(m.source_key);
                            }}
                            title="Add a follow-up task for this email"
                          >
                            Follow-up
                          </Button>
                          <Button
                            variant="danger"
                            size="sm"
                            busy={trashBusyId === m.source_key}
                            disabled={
                              smartDraftBusyId === m.source_key ||
                              followUpBusyId === m.source_key
                            }
                            onClick={(e) => {
                              e.stopPropagation();
                              void trashEmail(m);
                            }}
                            title="Move this message to Gmail Trash"
                          >
                            Trash
                          </Button>
                        </div>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>

          {selected ? (
            <div className="surface-card surface-flat surface-body-sky sticky top-16 self-start w-full shrink-0 rounded-lg border border-zinc-800 bg-zinc-900/60 p-4 lg:w-105 max-h-[calc(100vh-5rem)] overflow-y-auto">
              <div className="mb-3 flex items-start justify-between gap-2">
                <CategoryBadge category={selected.resolved_category} />
                <Button
                  variant="ghost"
                  size="sm"
                  onClick={() => setSelectedId(null)}
                >
                  Close
                </Button>
              </div>
              <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1.5 text-xs">
                <dt className="text-zinc-400">From</dt>
                <dd className="break-all text-zinc-200">
                  {selected.from_addr ?? "—"}
                </dd>
                <dt className="text-zinc-400">To</dt>
                <dd className="break-all text-zinc-200">
                  {selected.to_addr ?? "—"}
                </dd>
                <dt className="text-zinc-400">Subject</dt>
                <dd className="text-zinc-200">
                  {selected.subject ?? "(no subject)"}
                </dd>
                <dt className="text-zinc-400">Date</dt>
                <dd className="text-zinc-200">
                  {formatDate(selected.internal_date_ms)}
                </dd>
                <dt className="text-zinc-400">Labels</dt>
                <dd className="text-zinc-200">
                  {selected.labels.length > 0 ? (
                    <>
                      <LabelChips labels={userLabels(selected.labels)} />
                      {selected.labels.some((l) => SYSTEM_LABELS.has(l)) ? (
                        <span
                          className={`text-xs text-zinc-400 ${
                            userLabels(selected.labels).length > 0 ? "ml-1.5" : ""
                          }`}
                        >
                          {selected.labels
                            .filter((l) => SYSTEM_LABELS.has(l))
                            .join(", ")}
                        </span>
                      ) : null}
                    </>
                  ) : (
                    "—"
                  )}
                </dd>
                <dt className="text-zinc-400">Rule</dt>
                <dd className="font-mono text-zinc-200">
                  {selected.matched_rule_id ?? "— (fallback)"}
                </dd>
                <dt className="text-zinc-400">Thread</dt>
                <dd className="break-all font-mono text-zinc-400">
                  {selected.thread_id ?? "—"}
                </dd>
              </dl>
              <div className="mt-3 flex flex-wrap gap-2 border-t border-zinc-800 pt-3">
                <Button
                  variant="primary"
                  size="sm"
                  busy={smartDraftBusyId === selected.source_key}
                  disabled={followUpBusyId === selected.source_key}
                  onClick={() => void startSmartDraft(selected)}
                  title="Prepare drafts from this email for review"
                >
                  {smartDraftBusyId === selected.source_key
                    ? "Drafting..."
                    : "Smart draft"}
                </Button>
                <Button
                  variant="secondary"
                  size="sm"
                  busy={followUpBusyId === selected.source_key}
                  disabled={smartDraftBusyId === selected.source_key}
                  onClick={() => void addFollowUp(selected.source_key)}
                  title="Add a follow-up task for this email"
                >
                  Follow-up
                </Button>
                <Button
                  variant="danger"
                  size="sm"
                  busy={trashBusyId === selected.source_key}
                  disabled={
                    smartDraftBusyId === selected.source_key ||
                    followUpBusyId === selected.source_key
                  }
                  onClick={() => void trashEmail(selected)}
                  title="Move this message to Gmail Trash"
                >
                  Trash
                </Button>
                {(["sender", "domain"] as const).map((scope) => {
                  const seed = ruleSeedFromMessage(selected, scope);
                  if (!seed) return null;
                  return (
                    <Button
                      key={scope}
                      variant="primary"
                      size="sm"
                      onClick={() => onCreateRule(seed)}
                      title={seed.conditions[0].value}
                    >
                      + Rule from {scope === "sender" ? "this sender" : "sender domain"}
                    </Button>
                  );
                })}
              </div>
              <CrmContextLinks
                sourceKey={selected.source_key}
                rawAddress={selected.from_addr}
                onUnauthorized={onUnauthorized}
              />
              <div className="mt-3 border-t border-zinc-800 pt-3">
                <div className="mb-1 text-xs font-semibold text-zinc-200">
                  Body preview
                </div>
                <EmailBodyPreview
                  body={selected.body_excerpt}
                  format={detectEmailBodyFormat(selected.body_excerpt)}
                  className="max-h-96 overflow-auto bg-zinc-950 p-3"
                />
              </div>
              {selected.attachments.length > 0 ? (
                <div className="mt-3 border-t border-zinc-800 pt-3">
                  <div className="mb-1 text-xs font-semibold text-zinc-200">
                    Attachments
                  </div>
                  <div className="flex flex-wrap gap-2">
                    {selected.attachments.map((attachment) => (
                      <span
                        key={attachment.attachment_id}
                        className="max-w-full rounded border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs text-zinc-300"
                        title={attachment.attachment_id}
                      >
                        {attachment.filename}
                        <span className="text-zinc-500">
                          {" "}
                          {attachment.mime_type ?? "unknown"} ·{" "}
                          {formatBytes(attachment.size_bytes)}
                        </span>
                      </span>
                    ))}
                  </div>
                </div>
              ) : null}
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}
