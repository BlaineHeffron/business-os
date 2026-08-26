import { useCallback, useEffect, useState } from "react";
import type { ClaudeSubscriptionAuthStartResponse } from "../../types/generated/ClaudeSubscriptionAuthStartResponse";
import type { ClaudeSubscriptionStatus } from "../../types/generated/ClaudeSubscriptionStatus";
import type { LlmRouteSettingsResponse } from "../../types/generated/LlmRouteSettingsResponse";
import { api, errorMessage, isRevisionConflict, isUnauthorized } from "../../lib/api";
import { Button, Card, StatusBadge, cellCls, rowDivideCls, rowHoverCls, tableCls, tableWrapCls, theadCls } from "../../components/ui";

const KNOWN_MODELS: Record<string, string[]> = {
  anthropic: ["claude-opus-4-8", "claude-sonnet-4-6", "claude-haiku-4-5"],
  openai: ["gpt-4o", "gpt-4o-mini", "gpt-4-turbo", "o1", "o1-mini"],
  openrouter: [],
};

const CUSTOM_SENTINEL = "__custom__";
type Backend = "api" | "harness";

function backend(raw: string): Backend {
  return raw === "harness" ? "harness" : "api";
}

function modelProviderForBackend(routeBackend: Backend, apiProvider: string): string {
  return routeBackend === "harness" ? "anthropic" : apiProvider;
}

function ModelSelect({
  value,
  onChange,
  provider,
  disabled,
  placeholder,
}: {
  value: string;
  onChange: (v: string) => void;
  provider: string;
  disabled?: boolean;
  placeholder?: string;
}) {
  const knownList = KNOWN_MODELS[provider] ?? [];
  const valueIsKnown = value === "" || knownList.includes(value);
  // Track whether the user has clicked "custom…" explicitly
  const [customMode, setCustomMode] = useState(!valueIsKnown);

  if (knownList.length === 0) {
    const cls =
      "rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-100 disabled:opacity-50 focus:border-sky-600 focus:outline-none";
    return (
      <input
        disabled={disabled}
        className={`${cls} min-w-52`}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder ?? "model id"}
      />
    );
  }

  const showCustomInput = customMode || !valueIsKnown;
  const selectValue = showCustomInput ? CUSTOM_SENTINEL : value;

  const cls =
    "rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-100 disabled:opacity-50 focus:border-sky-600 focus:outline-none";

  return (
    <div className="flex gap-1">
      <select
        disabled={disabled}
        className={cls}
        value={selectValue}
        onChange={(e) => {
          if (e.target.value === CUSTOM_SENTINEL) {
            setCustomMode(true);
            if (valueIsKnown) onChange("");
          } else {
            setCustomMode(false);
            onChange(e.target.value);
          }
        }}
      >
        <option value="">{placeholder ?? "use default"}</option>
        {knownList.map((m) => (
          <option key={m} value={m}>
            {m}
          </option>
        ))}
        {knownList.length > 0 && (
          <option value={CUSTOM_SENTINEL}>custom…</option>
        )}
      </select>
      {showCustomInput && (
        <input
          disabled={disabled}
          className={`${cls} min-w-52`}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder="model id"
          // eslint-disable-next-line jsx-a11y/no-autofocus
          autoFocus={customMode && valueIsKnown}
        />
      )}
    </div>
  );
}

type SettingsDraft = {
  backend: Backend;
  model: string;
  max_tokens: number;
  timeout_ms: number;
  overrides: Record<string, { enabled: boolean; backend: Backend; model: string }>;
};

function draftFromSettings(settings: LlmRouteSettingsResponse): SettingsDraft {
  const overrides: SettingsDraft["overrides"] = {};
  for (const purpose of settings.purposes) {
    overrides[purpose.purpose] = {
      enabled: purpose.override_backend != null,
      backend: backend(purpose.override_backend ?? purpose.effective_backend),
      model: purpose.override_model ?? "",
    };
  }
  return {
    backend: backend(settings.global.backend),
    model: settings.global.model ?? "",
    max_tokens: settings.global.max_tokens,
    timeout_ms: settings.global.timeout_ms,
    overrides,
  };
}

export function AiSettings({ onUnauthorized }: { onUnauthorized: () => void }) {
  const [settings, setSettings] = useState<LlmRouteSettingsResponse | null>(null);
  const [draft, setDraft] = useState<SettingsDraft | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<{
    kind: "ok" | "conflict" | "error";
    text: string;
  } | null>(null);
  const [saving, setSaving] = useState(false);
  const [subscription, setSubscription] =
    useState<ClaudeSubscriptionStatus | null>(null);
  const [authFlow, setAuthFlow] =
    useState<ClaudeSubscriptionAuthStartResponse | null>(null);
  const [authorizationCode, setAuthorizationCode] = useState("");
  const [authBusy, setAuthBusy] = useState(false);
  const [authNotice, setAuthNotice] = useState<{
    kind: "ok" | "error" | "info";
    text: string;
  } | null>(null);

  const loadSettings = useCallback(async () => {
    try {
      const routeSettings = await api.llmSettings();
      setSettings(routeSettings);
      setDraft(draftFromSettings(routeSettings));
      setError(null);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    }
  }, [onUnauthorized]);

  const loadSubscription = useCallback(async () => {
    try {
      const status = await api.claudeSubscriptionStatus();
      setSubscription(status);
      if (status.connected) {
        setAuthFlow(null);
        setAuthorizationCode("");
      }
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else {
        setAuthNotice({
          kind: "error",
          text: `Could not check Claude subscription: ${errorMessage(err)}`,
        });
      }
    }
  }, [onUnauthorized]);

  useEffect(() => {
    void loadSettings();
    void loadSubscription();
  }, [loadSettings, loadSubscription]);

  const startSubscriptionAuth = async () => {
    const authWindow = window.open("about:blank", "_blank");
    setAuthBusy(true);
    setAuthNotice(null);
    try {
      const flow = await api.startClaudeSubscriptionAuth({
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setAuthFlow(flow);
      if (authWindow) {
        authWindow.opener = null;
        authWindow.location.href = flow.authorization_url;
      }
      setAuthNotice({
        kind: "info",
        text: "Finish signing in with Claude, then paste the one-time authorization code below.",
      });
      await loadSubscription();
    } catch (err) {
      authWindow?.close();
      if (isUnauthorized(err)) onUnauthorized();
      else {
        setAuthNotice({
          kind: "error",
          text: `Could not start Claude sign-in: ${errorMessage(err)}`,
        });
      }
    } finally {
      setAuthBusy(false);
    }
  };

  const completeSubscriptionAuth = async () => {
    if (!authFlow || !authorizationCode.trim()) return;
    setAuthBusy(true);
    setAuthNotice(null);
    try {
      await api.completeClaudeSubscriptionAuth({
        flow_id: authFlow.flow_id,
        authorization_code: authorizationCode.trim(),
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setAuthorizationCode("");
      setAuthNotice({
        kind: "info",
        text: "Authorization submitted. Checking the Claude connection…",
      });
      for (let attempt = 0; attempt < 15; attempt += 1) {
        await new Promise((resolve) => window.setTimeout(resolve, 1_000));
        const status = await api.claudeSubscriptionStatus();
        setSubscription(status);
        if (status.connected) {
          setAuthFlow(null);
          setAuthNotice({
            kind: "ok",
            text: "Claude subscription connected.",
          });
          return;
        }
        if (!status.authorization_pending) break;
      }
      setAuthNotice({
        kind: "error",
        text: "Claude did not confirm the connection. Start a fresh sign-in and try the new code.",
      });
      setAuthFlow(null);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else {
        setAuthNotice({
          kind: "error",
          text: `Claude sign-in failed: ${errorMessage(err)}`,
        });
      }
    } finally {
      setAuthBusy(false);
    }
  };

  const saveSettings = async () => {
    if (!settings || !draft) return;
    // Without a harness on this instance, "api" is the only valid backend —
    // the selector is hidden, so pin the payload to api regardless of draft.
    const harnessAvailable = settings.harness_available;
    setSaving(true);
    setNotice(null);
    try {
      await api.updateLlmSettings({
        expected_revision: settings.revision ?? null,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
        global: {
          backend: harnessAvailable ? draft.backend : "api",
          model: draft.model.trim() || null,
          max_tokens: draft.max_tokens,
          timeout_ms: draft.timeout_ms,
        },
        overrides: settings.purposes
          .map((purpose) => {
            const value = draft.overrides[purpose.purpose];
            if (!value?.enabled) return null;
            return {
              purpose: purpose.purpose,
              backend: harnessAvailable ? value.backend : "api",
              model: value.model.trim() || null,
            };
          })
          .filter((value): value is NonNullable<typeof value> => value !== null),
      });
      setNotice({ kind: "ok", text: "LLM settings saved." });
      await loadSettings();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else if (isRevisionConflict(err)) {
        await loadSettings();
        setNotice({
          kind: "conflict",
          text: "Changed elsewhere — reloaded. Review and save again.",
        });
      } else setNotice({ kind: "error", text: `Save failed: ${errorMessage(err)}` });
    } finally {
      setSaving(false);
    }
  };

  if (error) {
    return (
      <div className="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300">
        Failed to load AI settings: {error}
      </div>
    );
  }
  if (!settings || !draft) {
    return <div className="text-sm text-zinc-500">Loading…</div>;
  }

  // The api/harness backend selector only makes sense where a Claude CLI
  // harness actually runs (operator/dev). On a deployed client instance it's
  // absent, so we hide the selector and treat the backend as "api".
  const harnessAvailable = settings.harness_available;
  const globalBackend: Backend = harnessAvailable ? draft.backend : "api";

  return (
    <div className="space-y-4">
      {harnessAvailable ? (
        <Card className="surface-flat surface-body-zinc">
          <div className="surface-section-head surface-head-zinc flex flex-wrap items-center justify-between gap-3">
            <div>
              <div className="text-xs font-semibold uppercase tracking-wide text-zinc-500">
                Claude subscription
              </div>
              <div className="mt-1 text-sm text-zinc-300">
                Uses Claude Code with a Claude.ai subscription. No API key is required.
              </div>
            </div>
            {subscription ? (
              <StatusBadge
                tone={
                  subscription.connected
                    ? "ok"
                    : subscription.authorization_pending
                      ? "progress"
                      : "warning"
                }
                pulse={subscription.authorization_pending}
              >
                {subscription.connected
                  ? "Connected"
                  : subscription.authorization_pending
                    ? "Sign-in pending"
                    : "Needs sign-in"}
              </StatusBadge>
            ) : (
              <StatusBadge tone="neutral">Checking…</StatusBadge>
            )}
          </div>

          <div className="mt-3 flex flex-wrap items-center gap-2">
            <Button
              variant={subscription?.connected ? "secondary" : "primary"}
              size="sm"
              busy={authBusy}
              disabled={subscription != null && !subscription.available}
              onClick={() => void startSubscriptionAuth()}
            >
              {subscription?.connected ? "Refresh sign-in" : "Connect subscription"}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              disabled={authBusy}
              onClick={() => void loadSubscription()}
            >
              Check status
            </Button>
            {subscription?.connected ? (
              <span className="text-xs text-zinc-500">
                {subscription.auth_method ?? "claude.ai"}
                {subscription.subscription_type
                  ? ` · ${subscription.subscription_type} plan`
                  : ""}
              </span>
            ) : null}
          </div>

          {authFlow ? (
            <div className="mt-3 rounded-md border border-zinc-800 bg-zinc-950/60 p-3">
              <div className="text-sm font-medium text-zinc-200">
                Complete Claude authorization
              </div>
              <p className="mt-1 text-xs leading-relaxed text-zinc-500">
                Finish the Claude sign-in page, copy its one-time code, and paste it
                here. BusinessOS sends the code directly to Claude Code and never
                stores it.
              </p>
              <div className="mt-3 flex flex-col gap-2 sm:flex-row">
                <input
                  type="password"
                  autoComplete="off"
                  aria-label="Claude authorization code"
                  className="min-w-0 flex-1 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 focus:border-sky-600 focus:outline-none"
                  value={authorizationCode}
                  onChange={(event) => setAuthorizationCode(event.target.value)}
                  placeholder="Paste one-time authorization code"
                />
                <Button
                  variant="primary"
                  size="sm"
                  busy={authBusy}
                  disabled={!authorizationCode.trim()}
                  onClick={() => void completeSubscriptionAuth()}
                >
                  Complete connection
                </Button>
              </div>
              <a
                className="mt-2 inline-block text-xs text-sky-400 hover:text-sky-300"
                href={authFlow.authorization_url}
                target="_blank"
                rel="noreferrer"
              >
                Reopen Claude sign-in
              </a>
            </div>
          ) : null}

          {authNotice ? (
            <div
              className={`mt-3 rounded-md border px-3 py-2 text-sm ${
                authNotice.kind === "ok"
                  ? "border-emerald-900/60 bg-emerald-950/30 text-emerald-300"
                  : authNotice.kind === "info"
                    ? "border-sky-900/60 bg-sky-950/30 text-sky-300"
                    : "border-red-900/60 bg-red-950/40 text-red-300"
              }`}
            >
              {authNotice.text}
            </div>
          ) : null}
        </Card>
      ) : null}

    <Card className="surface-flat surface-body-zinc">
      <div className="surface-section-head surface-head-zinc mb-3 flex items-center justify-between gap-3">
        <div>
          <div className="text-xs font-semibold uppercase tracking-wide text-zinc-500">
            LLM routing
          </div>
          <div className="mt-1 text-sm text-zinc-300">
            Provider {settings.api_provider} · defaults from {settings.global.source}
          </div>
        </div>
        <Button
          variant="primary"
          size="sm"
          busy={saving}
          onClick={() => void saveSettings()}
        >
          Save
        </Button>
      </div>
      {notice ? (
        <div
          className={`mb-3 rounded-md border px-3 py-2 text-sm ${
            notice.kind === "ok"
              ? "border-emerald-900/60 bg-emerald-950/30 text-emerald-300"
              : notice.kind === "conflict"
                ? "border-amber-900/60 bg-amber-950/30 text-amber-300"
              : "border-red-900/60 bg-red-950/40 text-red-300"
          }`}
        >
          {notice.text}
        </div>
      ) : null}
      <div className="grid gap-3 md:grid-cols-4">
        {harnessAvailable ? (
          <label className="text-xs font-medium text-zinc-400">
            Default backend
            <select
              className="mt-1 w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-100 focus:border-sky-600 focus:outline-none"
              value={draft.backend}
              onChange={(e) =>
                setDraft({ ...draft, backend: backend(e.target.value) })
              }
            >
              <option value="api">API</option>
              <option value="harness">Harness</option>
            </select>
          </label>
        ) : null}
        <label className="text-xs font-medium text-zinc-400 md:col-span-2">
          Default model
          <div className="mt-1">
            <ModelSelect
              value={draft.model}
              onChange={(v) => setDraft({ ...draft, model: v })}
              provider={modelProviderForBackend(globalBackend, settings.api_provider)}
              placeholder="provider default"
            />
          </div>
        </label>
        <div className="grid grid-cols-2 gap-2">
          <label className="text-xs font-medium text-zinc-400">
            Max tokens
            <input
              type="number"
              min={256}
              max={65536}
              className="mt-1 w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm tabular-nums text-zinc-100 focus:border-sky-600 focus:outline-none"
              value={draft.max_tokens}
              onChange={(e) => setDraft({ ...draft, max_tokens: Number(e.target.value) })}
            />
          </label>
          <label className="text-xs font-medium text-zinc-400">
            Timeout ms
            <input
              type="number"
              min={5000}
              max={600000}
              step={1000}
              className="mt-1 w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm tabular-nums text-zinc-100 focus:border-sky-600 focus:outline-none"
              value={draft.timeout_ms}
              onChange={(e) => setDraft({ ...draft, timeout_ms: Number(e.target.value) })}
            />
          </label>
        </div>
      </div>
      <div className={`mt-4 ${tableWrapCls} surface-flat surface-body-zinc`}>
        <table className={tableCls}>
          <thead className={`${theadCls} surface-head-zinc border-b border-zinc-800`}>
            <tr>
              <th className={`${cellCls} font-medium`}>task</th>
              <th className={`${cellCls} font-medium`}>effective</th>
              <th className={`${cellCls} font-medium`}>override</th>
              <th className={`${cellCls} font-medium`}>model</th>
            </tr>
          </thead>
          <tbody className={rowDivideCls}>
            {settings.purposes.map((purpose) => {
              const value = draft.overrides[purpose.purpose] ?? {
                enabled: false,
                backend: backend(purpose.effective_backend),
                model: "",
              };
              return (
                <tr key={purpose.purpose} className={rowHoverCls}>
                  <td className={cellCls}>
                    <div className="font-medium text-zinc-100">{purpose.label}</div>
                    <div className="mt-0.5 max-w-xl text-xs leading-snug text-zinc-500">
                      {purpose.description}
                    </div>
                    <div className="mt-1 font-mono text-xs text-zinc-500">
                      {purpose.purpose}
                    </div>
                  </td>
                  <td className={`${cellCls} text-zinc-300`}>
                    {purpose.effective_backend}
                    {purpose.effective_model ? ` · ${purpose.effective_model}` : ""}
                  </td>
                  <td className={cellCls}>
                    <div className="flex items-center gap-2">
                      <input
                        type="checkbox"
                        aria-label={`Override ${purpose.label}`}
                        checked={value.enabled}
                        onChange={(e) =>
                          setDraft({
                            ...draft,
                            overrides: {
                              ...draft.overrides,
                              [purpose.purpose]: {
                                ...value,
                                enabled: e.target.checked,
                              },
                            },
                          })
                        }
                      />
                      {harnessAvailable ? (
                        <select
                          disabled={!value.enabled}
                          className="rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-100 disabled:opacity-50"
                          value={value.backend}
                          onChange={(e) =>
                            setDraft({
                              ...draft,
                              overrides: {
                                ...draft.overrides,
                                [purpose.purpose]: {
                                  ...value,
                                  backend: backend(e.target.value),
                                },
                              },
                            })
                          }
                        >
                          <option value="api">API</option>
                          <option value="harness">Harness</option>
                        </select>
                      ) : (
                        <span className="text-xs text-zinc-500">
                          {value.enabled ? "model override" : "use default"}
                        </span>
                      )}
                    </div>
                  </td>
                  <td className={cellCls}>
                    <ModelSelect
                      disabled={!value.enabled}
                      value={value.model}
                      onChange={(v) =>
                        setDraft({
                          ...draft,
                          overrides: {
                            ...draft.overrides,
                            [purpose.purpose]: { ...value, model: v },
                          },
                        })
                      }
                      provider={modelProviderForBackend(
                        harnessAvailable ? value.backend : "api",
                        settings.api_provider,
                      )}
                      placeholder="use backend/default model"
                    />
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </Card>
    </div>
  );
}
