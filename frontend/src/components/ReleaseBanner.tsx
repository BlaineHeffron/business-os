import { useEffect, useState } from "react";
import type { ReleaseNote } from "../types/generated/ReleaseNote";
import { api, errorMessage, isUnauthorized } from "../lib/api";
import { Button } from "./ui";

export default function ReleaseBanner({
  onUnauthorized,
}: {
  onUnauthorized: () => void;
}) {
  const [note, setNote] = useState<ReleaseNote | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [dismissing, setDismissing] = useState(false);

  useEffect(() => {
    let cancelled = false;
    api
      .latestReleaseNote()
      .then((response) => {
        if (cancelled) return;
        setNote(response.notes[0] ?? null);
        setError(null);
      })
      .catch((err) => {
        if (cancelled) return;
        if (isUnauthorized(err)) {
          onUnauthorized();
          return;
        }
        setError(errorMessage(err));
      });
    return () => {
      cancelled = true;
    };
  }, [onUnauthorized]);

  if (!note) return null;

  const dismiss = async () => {
    setDismissing(true);
    setError(null);
    try {
      await api.dismissReleaseNote(note.release_note_id, {
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNote(null);
    } catch (err) {
      if (isUnauthorized(err)) {
        onUnauthorized();
        return;
      }
      setError(errorMessage(err));
    } finally {
      setDismissing(false);
    }
  };

  return (
    <div className="mb-4 rounded-lg border border-sky-200 bg-sky-50 px-4 py-3 text-sm text-sky-950 shadow-sm dark:border-sky-900/60 dark:bg-sky-950/35 dark:text-sky-100">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <div className="font-semibold">{note.title || "What's new"}</div>
          <div className="mt-1 leading-6 text-sky-900 dark:text-sky-100">
            {note.summary}
          </div>
          {note.body ? (
            <div className="mt-2 whitespace-pre-line leading-6 text-sky-900/80 dark:text-sky-100/80">
              {note.body}
            </div>
          ) : null}
          {error ? (
            <div className="mt-2 text-xs font-medium text-red-700 dark:text-red-300">
              {error}
            </div>
          ) : null}
        </div>
        <Button
          variant="secondary"
          size="sm"
          onClick={dismiss}
          disabled={dismissing}
          className="shrink-0 border-sky-200 bg-white text-sky-950 hover:bg-sky-100 dark:border-sky-800 dark:bg-sky-950 dark:text-sky-100 dark:hover:bg-sky-900"
        >
          {dismissing ? "Closing…" : "Dismiss"}
        </Button>
      </div>
    </div>
  );
}
