import { useCallback, useEffect, useState } from "react";
import type { CallInputsDriveSettingsResponse } from "../../types/generated/CallInputsDriveSettingsResponse";
import type { GoogleDriveFolderOption } from "../../types/generated/GoogleDriveFolderOption";
import { api, errorMessage, isRevisionConflict, isUnauthorized } from "../../lib/api";
import DriveFolderSelect from "../../components/DriveFolderSelect";
import { Button, Card } from "../../components/ui";

export function CallSettings({ onUnauthorized }: { onUnauthorized: () => void }) {
  const [settings, setSettings] =
    useState<CallInputsDriveSettingsResponse | null>(null);
  const [selected, setSelected] = useState<GoogleDriveFolderOption | null>(null);
  const [cleared, setCleared] = useState(false);
  const [intervalSecs, setIntervalSecs] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ kind: "ok" | "error"; text: string } | null>(
    null,
  );
  const [saving, setSaving] = useState(false);

  const load = useCallback(async () => {
    try {
      const next = await api.callInputsDriveSettings();
      setSettings(next);
      setSelected(null);
      setCleared(false);
      setIntervalSecs(next.interval_secs != null ? String(next.interval_secs) : "");
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
    const folderId = cleared ? null : (selected?.folder_id ?? settings.drive_folder_id ?? null);
    const folderName = cleared ? null : (selected?.name ?? settings.drive_folder_name ?? null);
    const trimmedInterval = intervalSecs.trim();
    let parsedInterval: number | null = null;
    if (trimmedInterval !== "") {
      const n = Number(trimmedInterval);
      if (!Number.isInteger(n) || n < 60 || n > 86400) {
        setNotice({
          kind: "error",
          text: "Schedule interval must be 60-86400 seconds, or blank for the server default.",
        });
        return;
      }
      parsedInterval = n;
    }
    setSaving(true);
    setNotice(null);
    try {
      await api.updateCallInputsDriveSettings({
        expected_revision: settings.revision ?? null,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
        drive_folder_id: folderId,
        drive_folder_name: folderName,
        ingestion_enabled: folderId !== null,
        interval_secs: parsedInterval,
      });
      setNotice({ kind: "ok", text: "Audio recording settings saved." });
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
        Failed to load call settings: {error}
      </div>
    );
  }
  if (!settings) {
    return <div className="text-sm text-zinc-500">Loading…</div>;
  }

  const selectedFolderId = cleared
    ? null
    : (selected?.folder_id ?? settings.drive_folder_id ?? null);
  const selectedFolderName = cleared
    ? null
    : (selected?.name ?? settings.drive_folder_name ?? null);
  const pickerDisabled =
    !settings.credential_connected || settings.drive_scope_granted === false;

  return (
    <Card className="surface-flat surface-body-zinc">
      <div className="surface-section-head surface-head-zinc mb-3 flex items-center justify-between gap-3">
        <div>
          <div className="text-xs font-semibold uppercase tracking-wide text-zinc-500">
            Call audio intake
          </div>
          <div className="mt-1 text-sm text-zinc-300">
            Google Drive folder watched for approved recordings before local transcription.
          </div>
        </div>
        <Button variant="primary" size="sm" busy={saving} onClick={() => void save()}>
          Save
        </Button>
      </div>
      {!settings.credential_connected ? (
        <div className="mb-3 rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-2 text-sm text-amber-200">
          Connect Google before choosing a Drive folder.
        </div>
      ) : settings.drive_scope_granted === false ? (
        <div className="mb-3 rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-2 text-sm text-amber-200">
          Reconnect Google to grant Drive folder access.
        </div>
      ) : null}
      <DriveFolderSelect
        selectedFolderId={selectedFolderId}
        selectedFolderName={selectedFolderName}
        disabled={pickerDisabled || saving}
        onUnauthorized={onUnauthorized}
        onSelect={(folder) => {
          setSelected(folder);
          setCleared(folder === null);
        }}
      />
      <div className="mt-4 grid gap-3 sm:grid-cols-[minmax(0,1fr)_220px]">
        <div className="rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2 text-sm text-zinc-300">
          {selectedFolderId
            ? "Recordings in this folder will be ingested automatically."
            : "Choose a folder to enable automatic recording ingest."}
        </div>
        <label className="flex flex-col gap-1">
          <span className="text-xs font-medium text-zinc-500">Every seconds</span>
          <input
            value={intervalSecs}
            disabled={saving}
            onChange={(event) => setIntervalSecs(event.target.value)}
            inputMode="numeric"
            placeholder="server default"
            className="rounded-md border border-zinc-700 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 placeholder:text-zinc-600 disabled:opacity-50 focus:border-sky-600 focus:outline-none"
          />
        </label>
      </div>
      {notice ? (
        <div
          className={`mt-3 rounded-md border px-3 py-2 text-sm ${
            notice.kind === "ok"
              ? "border-emerald-900/60 bg-emerald-950/30 text-emerald-200"
              : "border-red-900/60 bg-red-950/40 text-red-300"
          }`}
        >
          {notice.text}
        </div>
      ) : null}
    </Card>
  );
}
