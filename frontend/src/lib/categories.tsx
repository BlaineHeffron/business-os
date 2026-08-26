import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from "react";
import type { CategoryRecord } from "../types/generated/CategoryRecord";
import type { WorkQueuePolicy } from "../types/generated/WorkQueuePolicy";
import { api, isUnauthorized } from "./api";

/** category_id slug rule (mirrors the server): lowercase a-z, 0-9, _, max 64. */
export const CATEGORY_ID_PATTERN = /^[a-z0-9_]{1,64}$/;
export const CATEGORY_COLORS = [
  "#38bdf8",
  "#22c55e",
  "#f59e0b",
  "#f43f5e",
  "#a855f7",
  "#14b8a6",
];
export const DEFAULT_CATEGORY_COLOR = CATEGORY_COLORS[0];
export const FALLBACK_CATEGORY_ID = "inbound_email";

export function defaultWorkQueuePolicy(
  categoryId: string,
  autoProduce = false,
): WorkQueuePolicy {
  return {
    category_id: categoryId,
    create_work_item: false,
    packet_kinds: [],
    ai_suggestible_packet_kinds: [],
    ai_suggestible_gmail_scope: "default",
    ai_suggestible_gmail_categories: [],
    auto_produce: autoProduce,
  };
}

export function workQueuePolicyHasOutputs(policy: WorkQueuePolicy): boolean {
  return (
    policy.packet_kinds.length > 0 ||
    policy.ai_suggestible_packet_kinds.length > 0
  );
}

export function normalizeWorkQueuePolicy(
  policy: WorkQueuePolicy,
): WorkQueuePolicy {
  let normalized = policy;
  const canScopeAiTabs = policy.category_id === FALLBACK_CATEGORY_ID;
  if (
    !canScopeAiTabs &&
    (policy.ai_suggestible_gmail_scope !== "default" ||
      policy.ai_suggestible_gmail_categories.length > 0)
  ) {
    normalized = {
      ...normalized,
      ai_suggestible_gmail_scope: "default",
      ai_suggestible_gmail_categories: [],
    };
  }
  if (
    normalized.ai_suggestible_packet_kinds.length === 0 &&
    (normalized.ai_suggestible_gmail_scope !== "default" ||
      normalized.ai_suggestible_gmail_categories.length > 0)
  ) {
    normalized = {
      ...normalized,
      ai_suggestible_gmail_scope: "default",
      ai_suggestible_gmail_categories: [],
    };
  }
  if (
    normalized.ai_suggestible_gmail_scope === "all" &&
    normalized.ai_suggestible_gmail_categories.length > 0
  ) {
    normalized = { ...normalized, ai_suggestible_gmail_categories: [] };
  }
  if (
    normalized.ai_suggestible_gmail_scope === "selected" &&
    normalized.ai_suggestible_gmail_categories.length === 0
  ) {
    normalized = {
      ...normalized,
      ai_suggestible_gmail_scope: "all",
    };
  }
  if (!normalized.create_work_item) {
    return {
      ...normalized,
      packet_kinds: [],
      ai_suggestible_packet_kinds: [],
      ai_suggestible_gmail_scope: "default",
      ai_suggestible_gmail_categories: [],
      auto_produce: false,
    };
  }
  return normalized;
}

export function slugifyCategoryId(raw: string): string {
  return raw
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "")
    .slice(0, 64);
}

export function nextCategorySort(categories: CategoryRecord[]): number {
  return categories.length > 0
    ? Math.max(...categories.map((category) => category.sort)) + 10
    : 0;
}

export function categoryLabel(category: CategoryRecord): string {
  return category.display_name.trim().length > 0
    ? category.display_name
    : category.category_id;
}

export interface CategoriesContextValue {
  categories: CategoryRecord[];
  refresh: () => Promise<void>;
}

const CategoriesContext = createContext<CategoriesContextValue>({
  categories: [],
  refresh: async () => {},
});

/**
 * Fetches the category registry once (and on demand via `refresh`) and shares
 * it app-wide so badges and selects stay in sync after mutations.
 */
export function CategoriesProvider({
  onUnauthorized,
  children,
}: {
  onUnauthorized: () => void;
  children: ReactNode;
}) {
  const [categories, setCategories] = useState<CategoryRecord[]>([]);

  const refresh = useCallback(async () => {
    try {
      const res = await api.categories();
      setCategories(
        [...res.categories].sort(
          (a, b) =>
            a.sort - b.sort || a.category_id.localeCompare(b.category_id),
        ),
      );
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      // On other failures keep the last known registry; badges fall back to
      // rendering raw ids.
    }
  }, [onUnauthorized]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  return (
    <CategoriesContext.Provider value={{ categories, refresh }}>
      {children}
    </CategoriesContext.Provider>
  );
}

export function useCategories(): CategoriesContextValue {
  return useContext(CategoriesContext);
}
