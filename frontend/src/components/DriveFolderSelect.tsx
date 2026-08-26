import { useCallback, useEffect, useState } from "react";
import type { GoogleDriveFolderOption } from "../types/generated/GoogleDriveFolderOption";
import { api, errorMessage, isUnauthorized } from "../lib/api";
import { Button } from "./ui";

export default function DriveFolderSelect({
  selectedFolderId,
  selectedFolderName,
  disabled,
  onSelect,
  onUnauthorized,
}: {
  selectedFolderId: string | null;
  selectedFolderName: string | null;
  disabled?: boolean;
  onSelect: (folder: GoogleDriveFolderOption | null) => void;
  onUnauthorized: () => void;
}) {
  const [query, setQuery] = useState("");
  const [folders, setFolders] = useState<GoogleDriveFolderOption[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    if (disabled) return;
    setLoading(true);
    setError(null);
    try {
      const next = await api.googleDriveFolders(query);
      setFolders(next.folders);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    } finally {
      setLoading(false);
    }
  }, [disabled, onUnauthorized, query]);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-col gap-2 sm:flex-row">
        <input
          aria-label="Search Drive folders"
          value={query}
          disabled={disabled}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Search Drive folders"
          className="min-w-0 flex-1 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 placeholder:text-zinc-600 disabled:opacity-50 focus:border-sky-600 focus:outline-none"
        />
        <Button size="sm" variant="secondary" busy={loading} disabled={disabled} onClick={() => void load()}>
          Search
        </Button>
      </div>
      {selectedFolderId ? (
        <div className="flex items-center justify-between gap-3 rounded-md border border-emerald-900/50 bg-emerald-950/20 px-3 py-2">
          <div className="min-w-0">
            <div className="truncate text-sm font-medium text-emerald-100">
              {selectedFolderName || selectedFolderId}
            </div>
            <div className="truncate text-xs text-emerald-300/70">{selectedFolderId}</div>
          </div>
          <Button size="sm" variant="ghost" disabled={disabled} onClick={() => onSelect(null)}>
            Clear
          </Button>
        </div>
      ) : null}
      {error ? (
        <div className="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300">
          Failed to load Drive folders: {error}
        </div>
      ) : null}
      <div className="max-h-72 overflow-auto rounded-md border border-zinc-800">
        {folders.length === 0 ? (
          <div className="px-3 py-3 text-sm text-zinc-500">
            {loading ? "Loading folders..." : "No folders found."}
          </div>
        ) : (
          folders.map((folder) => (
            <button
              key={folder.folder_id}
              type="button"
              disabled={disabled}
              onClick={() => onSelect(folder)}
              className={`flex w-full flex-col gap-1 border-b border-zinc-900 px-3 py-2 text-left last:border-b-0 hover:bg-zinc-900/70 disabled:opacity-50 ${
                selectedFolderId === folder.folder_id ? "bg-sky-950/30" : "bg-zinc-950"
              }`}
            >
              <span className="truncate text-sm font-medium text-zinc-100">{folder.name}</span>
              <span className="truncate text-xs text-zinc-500">{folder.folder_id}</span>
            </button>
          ))
        )}
      </div>
    </div>
  );
}
