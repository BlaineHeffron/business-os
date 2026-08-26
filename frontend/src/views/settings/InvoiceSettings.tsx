import { useCallback, useEffect, useState } from "react";
import type { InvoiceSettingsResponse } from "../../types/generated/InvoiceSettingsResponse";
import { api, errorMessage, isRevisionConflict, isUnauthorized } from "../../lib/api";
import { Button, Card } from "../../components/ui";

export function InvoiceSettings({ onUnauthorized }: { onUnauthorized: () => void }) {
  const [settings, setSettings] = useState<InvoiceSettingsResponse | null>(null);
  const [dueDays, setDueDays] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ kind: "ok" | "error"; text: string } | null>(
    null,
  );
  const [saving, setSaving] = useState(false);

  const load = useCallback(async () => {
    try {
      const next = await api.invoiceSettings();
      setSettings(next);
      setDueDays(next.default_due_days != null ? String(next.default_due_days) : "");
      setError(null);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    }
  }, [onUnauthorized]);

  useEffect(() => {
    void load();
  }, [load]);

  const save = async () => {
    if (!settings) return;
    const trimmed = dueDays.trim();
    let parsed: number | null = null;
    if (trimmed !== "") {
      const n = Number(trimmed);
      if (!Number.isInteger(n) || n < 1 || n > 365) {
        setNotice({
          kind: "error",
          text: "Default term must be a whole number 1–365 days, or blank for none.",
        });
        return;
      }
      parsed = n;
    }
    setSaving(true);
    setNotice(null);
    try {
      await api.updateInvoiceSettings({
        expected_revision: settings.revision ?? null,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
        default_due_days: parsed,
      });
      setNotice({ kind: "ok", text: "Invoicing settings saved." });
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
        Failed to load invoicing settings: {error}
      </div>
    );
  }
  if (!settings) {
    return <div className="text-sm text-zinc-500">Loading…</div>;
  }

  return (
    <Card className="surface-flat surface-body-zinc">
      <div className="surface-section-head surface-head-zinc mb-3 flex items-center justify-between gap-3">
        <div>
          <div className="text-xs font-semibold uppercase tracking-wide text-zinc-500">
            Invoice defaults
          </div>
          <div className="mt-1 text-sm text-zinc-300">
            Applied when a produced invoice has no due date from its source.
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
      <label className="block max-w-xs text-xs font-medium text-zinc-400">
        Default payment term (days)
        <input
          type="number"
          min={1}
          max={365}
          className="mt-1 w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm tabular-nums text-zinc-100 focus:border-sky-600 focus:outline-none"
          value={dueDays}
          onChange={(e) => setDueDays(e.target.value)}
          placeholder="blank = no default"
        />
      </label>
      <p className="mt-2 max-w-md text-xs leading-snug text-zinc-500">
        e.g. 30 for Net 30 — a produced invoice's due date becomes the draft date
        plus this many days when the source states no explicit date or "Net N"
        term. Leave blank to keep due dates empty. The operator can always edit a
        draft's due date before approval.
      </p>
    </Card>
  );
}
