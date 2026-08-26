import { useEffect, useState } from "react";

type DraftEntry = {
  revision: number;
  draft: { draft_id: string; status: string };
};

/**
 * Re-seeds the edit buffer whenever the staged draft (or its revision) changes.
 * "AI-produced fields remain editable until accepted."
 *
 * Returns [buffer, setBuffer]. Buffer is set to seed(active) when active is
 * staged, else null.
 */
export function useDraftEdit<E extends DraftEntry, T>(
  active: E | undefined,
  seed: (entry: E) => T,
): [T | null, React.Dispatch<React.SetStateAction<T | null>>] {
  const [buffer, setBuffer] = useState<T | null>(null);

  const draftKey = active
    ? `${active.draft.draft_id}:${active.revision}:${active.draft.status}`
    : "";

  useEffect(() => {
    setBuffer(active && active.draft.status === "staged" ? seed(active) : null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [draftKey]);

  return [buffer, setBuffer];
}
