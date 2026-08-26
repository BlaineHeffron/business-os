import { useCallback, useEffect, useState } from "react";
import type { EmailTriageGmailCategory } from "../../types/generated/EmailTriageGmailCategory";
import type { EmailTriageInboxSettingsResponse } from "../../types/generated/EmailTriageInboxSettingsResponse";
import type { WorkQueuePolicy } from "../../types/generated/WorkQueuePolicy";
import { api, errorMessage, isRevisionConflict, isUnauthorized } from "../../lib/api";
import { FALLBACK_CATEGORY_ID, defaultWorkQueuePolicy, normalizeWorkQueuePolicy } from "../../lib/categories";
import { Button, Card } from "../../components/ui";

const GMAIL_TABS: { id: EmailTriageGmailCategory; label: string }[] = [
  { id: "primary", label: "Primary" },
  { id: "updates", label: "Updates" },
  { id: "social", label: "Social" },
  { id: "promotions", label: "Promotions" },
  { id: "forums", label: "Forums" },
];

const AI_SUGGEST_ALL = "*";

export function InboxSettings({
  onUnauthorized,
  aiTriageEnabled,
}: {
  onUnauthorized: () => void;
  aiTriageEnabled: boolean;
}) {
  const [settings, setSettings] =
    useState<EmailTriageInboxSettingsResponse | null>(null);
  const [fallbackPolicy, setFallbackPolicy] = useState<WorkQueuePolicy | null>(null);
  const [visible, setVisible] = useState<Set<EmailTriageGmailCategory>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ kind: "ok" | "error"; text: string } | null>(
    null,
  );
  const [saving, setSaving] = useState(false);
  const [aiSaving, setAiSaving] = useState(false);

  const load = useCallback(async () => {
    try {
      const [next, policies] = await Promise.all([
        api.inboxSettings(),
        api.workQueuePolicies(),
      ]);
      const fallback =
        policies.policies.find((policy) => policy.category_id === FALLBACK_CATEGORY_ID) ??
        defaultWorkQueuePolicy(FALLBACK_CATEGORY_ID);
      setSettings(next);
      setFallbackPolicy(normalizeWorkQueuePolicy(fallback));
      setVisible(new Set(next.visible_gmail_categories));
      setError(null);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    }
  }, [onUnauthorized]);

  useEffect(() => {
    void load();
  }, [load]);

  const toggle = (category: EmailTriageGmailCategory) => {
    setVisible((current) => {
      const next = new Set(current);
      if (next.has(category)) next.delete(category);
      else next.add(category);
      return next;
    });
  };

  const save = async () => {
    if (!settings) return;
    const nextVisible = GMAIL_TABS.map((tab) => tab.id).filter((id) =>
      visible.has(id),
    );
    if (nextVisible.length === 0) {
      setNotice({ kind: "error", text: "Keep at least one Gmail tab visible." });
      return;
    }
    setSaving(true);
    setNotice(null);
    try {
      await api.updateInboxSettings({
        expected_revision: settings.revision ?? null,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
        visible_gmail_categories: nextVisible,
      });
      setNotice({ kind: "ok", text: "Inbox settings saved." });
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

  const updateFallbackPolicy = (next: WorkQueuePolicy) => {
    setFallbackPolicy(normalizeWorkQueuePolicy(next));
  };

  const saveFallbackAiPolicy = async () => {
    if (!fallbackPolicy) return;
    setAiSaving(true);
    setNotice(null);
    try {
      await api.upsertWorkQueuePolicy({
        policy: normalizeWorkQueuePolicy(fallbackPolicy),
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNotice({ kind: "ok", text: "AI triage scope saved." });
      await load();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice({ kind: "error", text: `Save failed: ${errorMessage(err)}` });
    } finally {
      setAiSaving(false);
    }
  };

  if (error) {
    return (
      <div className="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300">
        Failed to load inbox settings: {error}
      </div>
    );
  }
  if (!settings) {
    return <div className="text-sm text-zinc-500">Loading…</div>;
  }

  const aiOn = (fallbackPolicy?.ai_suggestible_packet_kinds.length ?? 0) > 0;
  const aiScope = fallbackPolicy?.ai_suggestible_gmail_scope ?? "default";
  const aiTabs = aiScope === "selected" || aiScope === "default"
    ? fallbackPolicy?.ai_suggestible_gmail_categories ?? []
    : [];

  return (
    <Card className="surface-flat surface-body-zinc space-y-5">
      <div className="surface-section-head surface-head-zinc mb-3 flex items-center justify-between gap-3">
        <div>
          <div className="text-xs font-semibold uppercase tracking-wide text-zinc-500">
            Gmail tabs
          </div>
          <div className="mt-1 text-sm text-zinc-300">
            Visible inbox tab filters. The tab rail is hidden when only one is enabled.
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
      <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
        {GMAIL_TABS.map((tab) => (
          <label
            key={tab.id}
            className="flex items-center justify-between rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm text-zinc-200"
          >
            <span>{tab.label}</span>
            <input
              type="checkbox"
              className="h-4 w-4 accent-sky-500"
              checked={visible.has(tab.id)}
              onChange={() => toggle(tab.id)}
            />
          </label>
        ))}
      </div>

      {aiTriageEnabled ? (
        <div className="border-t border-zinc-800 pt-4">
          <div className="mb-3 flex items-center justify-between gap-3">
            <div>
              <div className="text-xs font-semibold uppercase tracking-wide text-zinc-500">
                AI triage
              </div>
              <div className="mt-1 text-sm text-zinc-300">
                Which unmatched Gmail tabs may be examined by AI before they land in
                inbound_email.
              </div>
            </div>
            <Button
              variant="primary"
              size="sm"
              busy={aiSaving}
              disabled={!fallbackPolicy}
              onClick={() => void saveFallbackAiPolicy()}
            >
              Save
            </Button>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <button
              type="button"
              role="switch"
              aria-checked={aiOn}
              disabled={!fallbackPolicy || aiSaving}
              onClick={() => {
                if (!fallbackPolicy) return;
                const nextAiOn = !aiOn;
                updateFallbackPolicy({
                  ...fallbackPolicy,
                  create_work_item: nextAiOn ? true : fallbackPolicy.create_work_item,
                  ai_suggestible_packet_kinds: nextAiOn ? [AI_SUGGEST_ALL] : [],
                  ai_suggestible_gmail_scope: nextAiOn
                    ? fallbackPolicy.ai_suggestible_gmail_scope === "default" &&
                      fallbackPolicy.ai_suggestible_gmail_categories.length === 0
                      ? "all"
                      : fallbackPolicy.ai_suggestible_gmail_scope
                    : "default",
                  ai_suggestible_gmail_categories: nextAiOn
                    ? fallbackPolicy.ai_suggestible_gmail_categories
                    : [],
                });
              }}
              className={`rounded-full border px-2.5 py-1 text-xs transition disabled:opacity-40 ${
                aiOn
                  ? "border-violet-700 bg-violet-950/60 text-violet-300"
                  : "border-zinc-700 text-zinc-500 hover:bg-zinc-800 hover:text-zinc-200"
              }`}
            >
              AI triage {aiOn ? "on" : "off"}
            </button>
            {aiOn ? (
              <>
                <button
                  type="button"
                  disabled={!fallbackPolicy || aiSaving}
                  aria-pressed={aiScope === "all"}
                  title="Allow AI triage for all unmatched fallback mail"
                  onClick={() => {
                    if (!fallbackPolicy) return;
                    updateFallbackPolicy({
                      ...fallbackPolicy,
                      ai_suggestible_gmail_scope: "all",
                      ai_suggestible_gmail_categories: [],
                    });
                  }}
                  className={`rounded-full border px-2.5 py-1 text-xs transition disabled:opacity-40 ${
                    aiScope === "all"
                      ? "border-sky-700 bg-sky-950/60 text-sky-300"
                      : "border-zinc-700 text-zinc-500 hover:bg-zinc-800 hover:text-zinc-200"
                  }`}
                >
                  All tabs
                </button>
                {GMAIL_TABS.map((tab) => {
                  const selected = aiTabs.includes(tab.id);
                  return (
                    <button
                      key={tab.id}
                      type="button"
                      disabled={!fallbackPolicy || aiSaving}
                      aria-pressed={selected}
                      title={`Allow AI triage for ${tab.label} unmatched mail`}
                      onClick={() => {
                        if (!fallbackPolicy) return;
                        const next = selected
                          ? aiTabs.filter((id) => id !== tab.id)
                          : [...aiTabs, tab.id];
                        updateFallbackPolicy({
                          ...fallbackPolicy,
                          ai_suggestible_gmail_scope: "selected",
                          ai_suggestible_gmail_categories: next,
                        });
                      }}
                      className={`rounded-full border px-2.5 py-1 text-xs transition disabled:opacity-40 ${
                        selected
                          ? "border-sky-700 bg-sky-950/60 text-sky-300"
                          : "border-zinc-700 text-zinc-500 hover:bg-zinc-800 hover:text-zinc-200"
                      }`}
                    >
                      {tab.label}
                    </button>
                  );
                })}
              </>
            ) : null}
          </div>
        </div>
      ) : null}
    </Card>
  );
}
