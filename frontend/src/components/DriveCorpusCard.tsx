import { useCallback, useEffect, useRef, useState } from "react";
import type { DriveCorpusStatus } from "../types/generated/DriveCorpusStatus";
import type { DriveSearchHit } from "../types/generated/DriveSearchHit";
import { api, errorMessage, isUnauthorized } from "../lib/api";
import { Button, StatusBadge } from "./ui";

/** Drive corpus status: which folders feed the RAG index, whether the
 * connected Google credential carries drive.readonly, sync freshness and
 * index counts, a guarded Sync-now, and a test search over the local index.
 * Configuration itself is overlay/env (BOS_DRIVE_CORPUS_*) — this surface
 * makes the state visible, it doesn't edit it. */
export default function DriveCorpusCard({
  onUnauthorized,
}: {
  onUnauthorized: () => void;
}) {
  const [status, setStatus] = useState<DriveCorpusStatus | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<DriveSearchHit[] | null>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);

  const load = useCallback(async () => {
    try {
      setStatus(await api.driveCorpusStatus());
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice(`Failed to load corpus status: ${errorMessage(err)}`);
    }
  }, [onUnauthorized]);

  useEffect(() => {
    void load();
  }, [load]);

  const syncNow = async () => {
    setBusy(true);
    setNotice(null);
    try {
      await api.driveCorpusSyncNow();
      setNotice("Sync started — refresh in a bit to see progress.");
      setTimeout(() => void load(), 4_000);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice(`Sync not started: ${errorMessage(err)}`);
    } finally {
      setBusy(false);
    }
  };

  const search = async () => {
    if (!query.trim()) return;
    setBusy(true);
    setNotice(null);
    try {
      const res = await api.driveCorpusSearch(query, 5);
      setHits(res.hits);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice(`Search failed: ${errorMessage(err)}`);
    } finally {
      setBusy(false);
    }
  };

  const clearSearch = () => {
    setQuery("");
    setHits(null);
    searchInputRef.current?.focus();
  };

  if (!status) {
    return notice ? (
      <div className="mt-8 text-xs text-zinc-400">{notice}</div>
    ) : null;
  }

  const syncTone = status.in_flight
    ? "progress"
    : status.last_outcome === "error"
      ? "critical"
      : "ok";

  const counts = status.doc_counts;

  return (
    <section className="surface-card surface-flat surface-body-emerald mt-8 rounded-lg border border-zinc-800 bg-zinc-900/40 p-4">
      <div className="flex items-center gap-3">
        <h2 className="text-sm font-semibold text-zinc-200">Drive documents</h2>
        <StatusBadge tone={status.configured ? "ok" : "neutral"}>
          {status.configured ? "configured" : "not configured"}
        </StatusBadge>
        {status.configured ? (
          <Button
            variant="secondary"
            size="sm"
            onClick={() => void syncNow()}
            disabled={busy || status.in_flight}
            busy={busy && !status.in_flight}
            className="ml-auto"
          >
            {status.in_flight ? "Syncing…" : "Sync now"}
          </Button>
        ) : null}
      </div>

      {notice ? (
        <div className="mt-2 rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-1.5 text-xs text-amber-300">
          {notice}
        </div>
      ) : null}

      {!status.configured ? (
        <p className="mt-2 text-xs text-zinc-400">
          Select which Drive folders contain your source documents for content
          drafts. Ask your administrator to configure the folder list.
        </p>
      ) : (
        <div className="mt-2 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
          <span className="text-zinc-400">Folders</span>
          <span className="font-mono text-zinc-300">
            {status.folder_ids.join(", ") || "—"}
            {status.include_file_ids.length > 0
              ? ` (+${status.include_file_ids.length} pinned files)`
              : ""}
          </span>
          <span className="text-zinc-400">Credential</span>
          <span className="text-zinc-300">
            {!status.credential_connected ? (
              <span className="text-amber-300">
                No Google account connected — connect one above.
              </span>
            ) : status.drive_scope_granted === false ? (
              <span className="text-amber-300">
                Google is connected, but Drive access isn&apos;t granted. Reconnect
                Google to add Drive permission.
              </span>
            ) : (
              "connected"
            )}
          </span>
          <span className="text-zinc-400">Index</span>
          <span className="text-zinc-300">
            {String(counts.indexed)} documents · {String(counts.stale)} need
            refresh · {String(counts.skipped)} skipped · {String(counts.error)}{" "}
            errors · {String(status.chunk_count)} sections indexed
            {!status.backfill_complete ? (
              <span className="ml-2 text-amber-300">
                (initial sync in progress — give it a moment)
              </span>
            ) : null}
          </span>
          <span className="text-zinc-400">Sync</span>
          <span className="flex items-center gap-2 text-zinc-300">
            <StatusBadge tone={syncTone} pulse={status.in_flight}>
              {status.in_flight
                ? "syncing"
                : status.last_outcome === "error"
                  ? "error"
                  : "ok"}
            </StatusBadge>
            {status.sync_enabled ? "auto-sync on" : "auto-sync off — use Sync now above"}
            {status.last_outcome ? ` · last: ${status.last_outcome}` : ""}
            {status.last_error ? (
              <span className="ml-1 text-red-300">({status.last_error})</span>
            ) : null}
          </span>
        </div>
      )}

      {status.configured ? (
        <div className="mt-3">
          <div className="flex items-center gap-2">
            <input
              ref={searchInputRef}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void search();
                if (e.key === "Escape") clearSearch();
              }}
              placeholder="Test search the corpus…"
              className="w-64 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm text-zinc-200 placeholder:text-zinc-500 focus:border-sky-600 focus:outline-none"
            />
            <Button
              variant="secondary"
              size="sm"
              onClick={() => void search()}
              disabled={busy || !query.trim()}
              busy={busy}
            >
              Search
            </Button>
          </div>
          {hits != null ? (
            hits.length === 0 ? (
              <p className="mt-2 text-xs text-zinc-400">No matches.</p>
            ) : (
              <ul className="mt-2 flex flex-col gap-1.5">
                {hits.map((hit) => (
                  <li
                    key={hit.chunk_id}
                    className="rounded-md border border-zinc-800 bg-zinc-900/40 px-2 py-1.5 text-xs"
                  >
                    <span className="text-zinc-300">{hit.doc_title}</span>
                    {hit.heading_path.length > 0 ? (
                      <span className="text-zinc-400">
                        {" "}
                        — {hit.heading_path.join(" > ")}
                      </span>
                    ) : null}
                    <div className="mt-0.5 line-clamp-2 text-zinc-400">
                      {hit.text}
                    </div>
                  </li>
                ))}
              </ul>
            )
          ) : null}
        </div>
      ) : null}
    </section>
  );
}
