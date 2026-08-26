import { useCallback, useEffect, useState } from "react";
import type { HomeDashboardResponse } from "../../types/generated/HomeDashboardResponse";
import type { HomeDashboardWidgetKind } from "../../types/generated/HomeDashboardWidgetKind";
import type { HomeDashboardWidgetPreference } from "../../types/generated/HomeDashboardWidgetPreference";
import { api, errorMessage, isRevisionConflict, isUnauthorized } from "../../lib/api";
import { HOME_DASHBOARD_WIDGET_LABEL } from "../../lib/homeDashboard";
import { Card } from "../../components/ui";

export function HomeDashboardSettings({ onUnauthorized }: { onUnauthorized: () => void }) {
  const [data, setData] = useState<HomeDashboardResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [savingKind, setSavingKind] = useState<HomeDashboardWidgetKind | null>(null);

  const load = useCallback(async () => {
    try {
      setData(await api.homeDashboard());
      setError(null);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    }
  }, [onUnauthorized]);

  useEffect(() => {
    void load();
  }, [load]);

  const toggleWidget = async (kind: HomeDashboardWidgetKind) => {
    if (!data || savingKind) return;
    setSavingKind(kind);
    setNotice(null);
    const nextWidgets: HomeDashboardWidgetPreference[] = data.preferences.widgets.map((widget) =>
      widget.kind === kind ? { ...widget, enabled: !widget.enabled } : widget,
    );
    const snapshot = data;
    setData({ ...data, preferences: { ...data.preferences, widgets: nextWidgets } });
    try {
      const res = await api.updateHomeDashboardPreferences({
        widgets: nextWidgets,
        expected_revision: data.preferences.revision ?? null,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setData((current) =>
        current
          ? {
              ...current,
              preferences: {
                ...current.preferences,
                revision: res.revision ?? current.preferences.revision,
              },
            }
          : current,
      );
      void load();
    } catch (err) {
      if (isUnauthorized(err)) {
        onUnauthorized();
      } else if (isRevisionConflict(err)) {
        setNotice("Dashboard changed elsewhere — reloaded.");
        await load();
      } else {
        setData(snapshot);
        setNotice(`Dashboard preference failed: ${errorMessage(err)}`);
      }
    } finally {
      setSavingKind(null);
    }
  };

  if (error) {
    return (
      <div className="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300">
        Failed to load dashboard settings: {error}
      </div>
    );
  }
  if (!data) {
    return <div className="text-sm text-zinc-500">Loading…</div>;
  }

  const enabledKinds = new Set(
    data.preferences.widgets.filter((widget) => widget.enabled).map((widget) => widget.kind),
  );
  const availableKinds = new Set(data.available_widgets ?? []);

  return (
    <Card className="surface-flat surface-body-zinc">
      <div className="surface-section-head surface-head-zinc mb-3 flex items-center justify-between gap-3">
        <div>
          <div className="text-xs font-semibold uppercase tracking-wide text-zinc-500">
            Dashboard widgets
          </div>
          <div className="mt-1 text-sm text-zinc-300">
            Choose which enabled and available widgets appear on Home.
          </div>
        </div>
      </div>
      {notice ? (
        <div className="mb-3 rounded-md border border-amber-900/60 bg-amber-950/40 px-3 py-2 text-sm text-amber-200">
          {notice}
        </div>
      ) : null}
      <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
        {data.preferences.widgets.map((widget) => {
          const unavailable = !availableKinds.has(widget.kind);
          const enabled = enabledKinds.has(widget.kind) && !unavailable;
          return (
            <button
              key={widget.kind}
              type="button"
              disabled={savingKind !== null || unavailable}
              onClick={() => void toggleWidget(widget.kind)}
              className={`rounded-md border px-3 py-2 text-left text-sm transition focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70 disabled:cursor-not-allowed disabled:opacity-50 ${
                enabled
                  ? "border-sky-700 bg-sky-950/40 text-sky-200"
                  : "border-zinc-800 bg-zinc-950/40 text-zinc-400 hover:border-zinc-700 hover:text-zinc-200"
              }`}
              title={unavailable ? "Unavailable for this operator or client" : HOME_DASHBOARD_WIDGET_LABEL[widget.kind]}
              aria-pressed={enabled}
            >
              <span className="flex items-center justify-between gap-3">
                <span className="font-medium">
                  {HOME_DASHBOARD_WIDGET_LABEL[widget.kind]}
                </span>
                <span className="text-xs text-zinc-500">
                  {unavailable ? "Unavailable" : enabled ? "Shown" : "Hidden"}
                </span>
              </span>
            </button>
          );
        })}
      </div>
    </Card>
  );
}
