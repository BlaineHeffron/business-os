import { useCallback, useEffect, useMemo, useState } from "react";
import type { AdminSettingRow } from "../../types/generated/AdminSettingRow";
import type { AdminSettingsResponse } from "../../types/generated/AdminSettingsResponse";
import { api, errorMessage, isRevisionConflict, isUnauthorized } from "../../lib/api";
import {
  Button,
  Card,
  cellCls,
  rowDivideCls,
  rowHoverCls,
  tableCls,
  tableWrapCls,
  theadCls,
} from "../../components/ui";

function sourceLabel(source: AdminSettingRow["source"]): string {
  if (source === "stored_override") return "override";
  if (source === "overlay_default") return "overlay";
  if (source === "env_default") return "env/default";
  return "unset";
}

const systemSettingInputCls =
  "w-full min-w-48 rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 font-mono text-xs text-zinc-100 disabled:cursor-not-allowed disabled:opacity-55 focus:border-sky-600 focus:outline-none";

function boolValue(raw: string): boolean {
  return ["1", "true", "yes"].includes(raw.trim().toLowerCase());
}

function SystemSettingValueInput({
  row,
  value,
  disabled,
  onChange,
}: {
  row: AdminSettingRow;
  value: string;
  disabled: boolean;
  onChange: (value: string) => void;
}) {
  if (row.editable && row.value_kind === "bool") {
    return (
      <label className="inline-flex min-h-8 items-center gap-2 text-xs text-zinc-300">
        <input
          type="checkbox"
          disabled={disabled}
          checked={boolValue(value)}
          onChange={(event) => onChange(event.target.checked ? "1" : "0")}
          title={row.read_only_reason ?? undefined}
          className="h-4 w-4 rounded border-zinc-700 bg-zinc-950 text-sky-500 disabled:cursor-not-allowed disabled:opacity-55"
        />
        <span>{boolValue(value) ? "enabled" : "disabled"}</span>
      </label>
    );
  }

  if (row.editable && row.value_kind === "uint") {
    return (
      <input
        type="number"
        min={0}
        step={1}
        inputMode="numeric"
        disabled={disabled}
        className={systemSettingInputCls}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        title={row.read_only_reason ?? undefined}
      />
    );
  }

  if (row.editable && row.value_kind === "enum" && row.allowed_values?.length) {
    return (
      <select
        disabled={disabled}
        className={systemSettingInputCls}
        value={value}
        onChange={(event) => onChange(event.target.value)}
        title={row.read_only_reason ?? undefined}
      >
        {value === "" ? (
          <option value="" disabled>
            Select a value
          </option>
        ) : null}
        {row.allowed_values.map((option) => (
          <option key={option} value={option}>
            {option}
          </option>
        ))}
      </select>
    );
  }

  return (
    <input
      disabled={disabled}
      className={systemSettingInputCls}
      value={value}
      onChange={(event) => onChange(event.target.value)}
      title={row.read_only_reason ?? undefined}
    />
  );
}

export function SystemSettings({ onUnauthorized }: { onUnauthorized: () => void }) {
  const [settings, setSettings] = useState<AdminSettingsResponse | null>(null);
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<{
    kind: "ok" | "conflict" | "error";
    text: string;
  } | null>(null);
  const [busyVar, setBusyVar] = useState<string | null>(null);

  const loadSettings = useCallback(async () => {
    try {
      const response = await api.adminSettings();
      setSettings(response);
      setDrafts(
        Object.fromEntries(
          response.settings.map((row) => [row.name, row.effective_value ?? ""]),
        ),
      );
      setError(null);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    }
  }, [onUnauthorized]);

  useEffect(() => {
    void loadSettings();
  }, [loadSettings]);

  const grouped = useMemo(() => {
    const groups = new Map<string, AdminSettingRow[]>();
    for (const row of settings?.settings ?? []) {
      const rows = groups.get(row.group) ?? [];
      rows.push(row);
      groups.set(row.group, rows);
    }
    return Array.from(groups.entries());
  }, [settings]);

  const saveRow = async (row: AdminSettingRow) => {
    setBusyVar(row.name);
    setNotice(null);
    try {
      await api.updateAdminSetting(row.name, {
        expected_revision: row.revision ?? null,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
        value: (drafts[row.name] ?? "").trim(),
      });
      setNotice({ kind: "ok", text: `${row.name} saved.` });
      await loadSettings();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else if (isRevisionConflict(err)) {
        await loadSettings();
        setNotice({
          kind: "conflict",
          text: "Changed elsewhere — reloaded. Review and save again.",
        });
      } else {
        setNotice({ kind: "error", text: `Save failed: ${errorMessage(err)}` });
      }
    } finally {
      setBusyVar(null);
    }
  };

  const clearRow = async (row: AdminSettingRow) => {
    setBusyVar(row.name);
    setNotice(null);
    try {
      await api.clearAdminSetting(row.name, {
        expected_revision: row.revision ?? null,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNotice({ kind: "ok", text: `${row.name} cleared.` });
      await loadSettings();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else if (isRevisionConflict(err)) {
        await loadSettings();
        setNotice({
          kind: "conflict",
          text: "Changed elsewhere — reloaded. Review and save again.",
        });
      } else {
        setNotice({ kind: "error", text: `Clear failed: ${errorMessage(err)}` });
      }
    } finally {
      setBusyVar(null);
    }
  };

  if (error) {
    return (
      <div className="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300">
        Failed to load system settings: {error}
      </div>
    );
  }
  if (!settings) return <div className="text-sm text-zinc-500">Loading…</div>;

  return (
    <div className="flex flex-col gap-4">
      {notice ? (
        <div
          className={`rounded-md border px-3 py-2 text-sm ${
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
      {grouped.map(([group, rows]) => (
        <Card key={group} className="surface-flat surface-body-zinc">
          <div className="surface-section-head surface-head-zinc mb-3 flex items-center justify-between">
            <div className="text-sm font-semibold text-zinc-100">{group}</div>
            <div className="text-xs text-zinc-500">{rows.length} vars</div>
          </div>
          <div className={`${tableWrapCls} surface-flat surface-body-zinc`}>
            <table className={tableCls}>
              <thead className={`${theadCls} surface-head-zinc border-b border-zinc-800`}>
                <tr>
                  <th className={`${cellCls} font-medium`}>setting</th>
                  <th className={`${cellCls} font-medium`}>value</th>
                  <th className={`${cellCls} font-medium`}>source</th>
                  <th className={`${cellCls} font-medium`}>action</th>
                </tr>
              </thead>
              <tbody className={rowDivideCls}>
                {rows.map((row) => {
                  const originalValue = row.effective_value ?? "";
                  const draftValue = drafts[row.name] ?? originalValue;
                  const changed = draftValue.trim() !== originalValue;
                  const disabled = !row.editable || busyVar === row.name;
                  const value = row.secret
                    ? "••••••••"
                    : draftValue;
                  return (
                    <tr key={row.name} className={rowHoverCls}>
                      <td className={cellCls}>
                        <div className="font-mono text-xs text-zinc-200">{row.name}</div>
                        <div className="mt-1 max-w-2xl text-xs leading-snug text-zinc-500">
                          {row.description}
                        </div>
                        {row.read_only_reason ? (
                          <div className="mt-1 text-xs text-zinc-500">
                            {row.read_only_reason}
                          </div>
                        ) : null}
                      </td>
                      <td className={cellCls}>
                        <SystemSettingValueInput
                          row={row}
                          value={value}
                          disabled={disabled}
                          onChange={(nextValue) =>
                            setDrafts({ ...drafts, [row.name]: nextValue })
                          }
                        />
                        {row.default_value ? (
                          <div className="mt-1 font-mono text-xs text-zinc-600">
                            default {row.default_value}
                          </div>
                        ) : null}
                      </td>
                      <td className={`${cellCls} text-xs text-zinc-400`}>
                        {sourceLabel(row.source)}
                      </td>
                      <td className={cellCls}>
                        <div className="flex gap-2">
                          <Button
                            size="sm"
                            variant="secondary"
                            disabled={!row.editable || !changed}
                            busy={busyVar === row.name}
                            onClick={() => void saveRow(row)}
                          >
                            Save
                          </Button>
                          <Button
                            size="sm"
                            variant="ghost"
                            disabled={!row.editable || row.source !== "stored_override"}
                            onClick={() => void clearRow(row)}
                          >
                            Clear
                          </Button>
                        </div>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        </Card>
      ))}
    </div>
  );
}
