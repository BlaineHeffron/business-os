import { useCallback, useEffect, useState } from "react";
import type { DriveCorpusStatus } from "../../types/generated/DriveCorpusStatus";
import type { GoogleDriveFolderOption } from "../../types/generated/GoogleDriveFolderOption";
import type { SocialPublishingChannel } from "../../types/generated/SocialPublishingChannel";
import { api, errorMessage, isRevisionConflict, isUnauthorized } from "../../lib/api";
import DriveFolderSelect from "../../components/DriveFolderSelect";
import { Button, Card, SkeletonList, StatusBadge } from "../../components/ui";

type PublishingConnections = {
  blog: { available: boolean; live: boolean } | null;
  social: {
    configured: boolean;
    live: boolean;
    channels: SocialPublishingChannel[];
  } | null;
};

export function DriveCorpusSettings({ onUnauthorized }: { onUnauthorized: () => void }) {
  const [status, setStatus] = useState<DriveCorpusStatus | null>(null);
  const [selected, setSelected] = useState<GoogleDriveFolderOption | null>(null);
  const [cleared, setCleared] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<{ kind: "ok" | "error"; text: string } | null>(
    null,
  );
  const [saving, setSaving] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [connections, setConnections] = useState<PublishingConnections | null>(null);
  const [connectionsError, setConnectionsError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const next = await api.driveCorpusStatus();
      setStatus(next);
      setSelected(null);
      setCleared(false);
      setError(null);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    }
  }, [onUnauthorized]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    let cancelled = false;
    void Promise.allSettled([api.contentDrafts(), api.socialProposals()])
      .then(([contentResult, socialResult]) => {
        if (cancelled) return;
        const failures = [contentResult, socialResult].filter(
          (result): result is PromiseRejectedResult => result.status === "rejected",
        );
        if (failures.some((result) => isUnauthorized(result.reason))) {
          onUnauthorized();
          return;
        }
        setConnections({
          blog:
            contentResult.status === "fulfilled"
              ? {
                  available: contentResult.value.publishing_available,
                  live: contentResult.value.publishing_live_enabled,
                }
              : null,
          social:
            socialResult.status === "fulfilled"
              ? {
                  configured: socialResult.value.buffer_configured,
                  live: socialResult.value.buffer_live_enabled,
                  channels: socialResult.value.channels,
                }
              : null,
        });
        setConnectionsError(
          failures.length > 0
            ? failures.map((result) => errorMessage(result.reason)).join(" · ")
            : null,
        );
      });
    return () => {
      cancelled = true;
    };
  }, [onUnauthorized]);

  const save = async () => {
    if (!status) return;
    const folderId = cleared ? null : (selected?.folder_id ?? status.folder_ids[0] ?? null);
    const existingName =
      status.folder_names.find((folder) => folder.folder_id === folderId)?.name ?? null;
    const folderName = cleared ? null : (selected?.name ?? existingName);
    setSaving(true);
    setNotice(null);
    try {
      const response = await api.updateDriveCorpusSettings({
        expected_revision: status.revision ?? null,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
        drive_folder_id: folderId,
        drive_folder_name: folderName,
      });
      setNotice({ kind: "ok", text: driveCorpusSaveNotice(response) });
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

  const syncNow = async () => {
    setSyncing(true);
    setNotice(null);
    try {
      await api.driveCorpusSyncNow();
      setNotice({ kind: "ok", text: "Drive corpus sync started." });
      setTimeout(() => void load(), 4_000);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice({ kind: "error", text: `Sync not started: ${errorMessage(err)}` });
    } finally {
      setSyncing(false);
    }
  };

  if (error) {
    return (
      <div className="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300">
        Failed to load Drive corpus settings: {error}
      </div>
    );
  }
  if (!status) {
    return <SkeletonList rows={3} />;
  }

  const selectedFolderId = cleared ? null : (selected?.folder_id ?? status.folder_ids[0] ?? null);
  const selectedFolderName = cleared
    ? null
    : (selected?.name ??
      status.folder_names.find((folder) => folder.folder_id === selectedFolderId)?.name ??
      null);
  const pickerDisabled =
    status.folder_selection_pinned ||
    !status.credential_connected ||
    status.drive_scope_granted === false;

  return (
    <div id="drive-corpus-settings" className="flex flex-col gap-4">
      <PublishingConnectionsCard
        connections={connections}
        error={connectionsError}
      />
      <Card className="surface-flat surface-body-zinc">
      <div className="mb-4 flex flex-col items-stretch justify-between gap-3 sm:flex-row sm:items-center">
        <div className="min-w-0">
          <h2 className="text-xs font-semibold uppercase tracking-wide text-zinc-400">
            Research library
          </h2>
          <div className="mt-1 text-sm text-zinc-300">
            Google Drive folder used to ground content drafts with local document evidence.
          </div>
        </div>
        <div className="flex flex-wrap gap-2 sm:flex-none">
          <Button
            variant="secondary"
            size="sm"
            busy={syncing}
            disabled={!status.configured || status.in_flight}
            onClick={() => void syncNow()}
          >
            {status.in_flight ? "Syncing…" : "Sync now"}
          </Button>
          <Button
            variant="primary"
            size="sm"
            busy={saving}
            disabled={status.folder_selection_pinned}
            onClick={() => void save()}
          >
            Save
          </Button>
        </div>
      </div>
      {!status.credential_connected ? (
        <div className="mb-3 rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-2 text-sm text-amber-200">
          Connect Google before choosing a Drive folder.
        </div>
      ) : status.drive_scope_granted === false ? (
        <div className="mb-3 rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-2 text-sm text-amber-200">
          Reconnect Google to grant Drive folder access.
        </div>
      ) : status.folder_selection_pinned ? (
        <div className="mb-3 rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-2 text-sm text-amber-200">
          Folder selection is pinned by deployment config.
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
      <div className="mt-3 text-xs text-zinc-400">
        {String(status.doc_counts.indexed)} documents indexed ·{" "}
        {String(status.chunk_count)} sections ·{" "}
        {status.sync_enabled ? "auto-sync on" : "manual sync"}
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
    </div>
  );
}

function PublishingConnectionsCard({
  connections,
  error,
}: {
  connections: PublishingConnections | null;
  error: string | null;
}) {
  return (
    <Card className="surface-flat surface-body-zinc">
      <div className="mb-4">
        <h2 className="text-xs font-semibold uppercase tracking-wide text-zinc-400">
          Publishing connections
        </h2>
        <div className="mt-1 text-sm text-zinc-300">
          Read-only deployment status. Access tokens are redacted; credentials stay server-side.
        </div>
      </div>
      {!connections ? (
        <SkeletonList rows={2} />
      ) : (
        <div className="grid gap-3">
          {error ? (
            <div className="rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-2 text-sm text-amber-200">
              Some connection status is unavailable: {error}
            </div>
          ) : null}
          <div className="grid gap-3 lg:grid-cols-2">
            <ConnectionRow
              label="Blog publisher"
              readyLabel="Adapter ready"
              missingLabel="Adapter missing"
              configured={connections.blog?.available ?? null}
              live={connections.blog?.live ?? null}
              detail={
                !connections.blog
                  ? "Blog publishing status unavailable"
                  : connections.blog.available
                    ? "Client publisher adapter available"
                    : "No client publisher adapter configured"
              }
            />
            <ConnectionRow
              label="Buffer"
              readyLabel="Channels ready"
              missingLabel="Channels missing"
              configured={connections.social?.configured ?? null}
              live={connections.social?.live ?? null}
              detail={
                !connections.social
                  ? "Social publishing status unavailable"
                  : connections.social.channels.length > 0
                    ? `${connections.social.channels.length} configured destination${connections.social.channels.length === 1 ? "" : "s"}: ${connections.social.channels
                        .map((channel) => `${channel.name} (${channel.platform})`)
                        .join(" · ")}`
                    : "No social destinations configured"
              }
            />
          </div>
        </div>
      )}
    </Card>
  );
}

function ConnectionRow({
  label,
  readyLabel,
  missingLabel,
  configured,
  live,
  detail,
}: {
  label: string;
  readyLabel: string;
  missingLabel: string;
  configured: boolean | null;
  live: boolean | null;
  detail: string;
}) {
  return (
    <div className="rounded-md border border-zinc-800 bg-zinc-950/30 px-3 py-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 className="text-sm font-medium text-zinc-200">{label}</h3>
        <div className="flex flex-wrap gap-2">
          <StatusBadge tone={configured === null ? "neutral" : configured ? "ok" : "warning"}>
            {configured === null ? "Status unavailable" : configured ? readyLabel : missingLabel}
          </StatusBadge>
          <StatusBadge tone={live === null ? "neutral" : live ? "ok" : "warning"}>
            {live === null ? "Write gate unknown" : live ? "Live writes on" : "Live writes off · dry run"}
          </StatusBadge>
        </div>
      </div>
      <div className="mt-2 break-words text-xs text-zinc-400">{detail}</div>
    </div>
  );
}

function driveCorpusSaveNotice(response: {
  sync_started: boolean;
  sync_refusal_reason?: string | null;
}): string {
  if (response.sync_started) {
    return "Drive corpus folder saved. Syncing the selected folder now.";
  }
  switch (response.sync_refusal_reason) {
    case "drive_corpus_not_configured":
      return "Drive corpus folder saved. No sync started because no folder is selected.";
    case "sync_in_flight":
      return "Drive corpus folder saved. A sync is already running.";
    case "sync_cooldown":
      return "Drive corpus folder saved. Sync can be retried shortly.";
    case "sync_config_error":
    case "sync_spawn_failed":
      return "Drive corpus folder saved. Sync did not start; use Sync now to retry.";
    default:
      return "Drive corpus folder saved.";
  }
}
