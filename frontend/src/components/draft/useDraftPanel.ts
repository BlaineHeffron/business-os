import { useCallback, useEffect, useRef, useState } from "react";
import type { OutboxJobSummary } from "../../types/generated/OutboxJobSummary";
import type { MutationResponse } from "../../types/generated/MutationResponse";
import { api, errorCodeMessage, errorMessage, isRevisionConflict, isUnauthorized } from "../../lib/api";

type DraftEntry = {
  revision: number;
  draft: { draft_id: string; status: string };
  outbox_job?: OutboxJobSummary | null;
};
type DraftsResponse<E> = {
  drafts: E[];
  publishing_available?: boolean;
  publishing_live_enabled?: boolean;
};

export type DraftPanelNotice = { text: string; kind: "error" | "conflict" } | null;

export interface UseDraftPanelArgs<E extends DraftEntry> {
  itemId: string;
  produceKind: string;
  onUnauthorized: () => void;
  fetchDrafts: (itemId: string) => Promise<DraftsResponse<E>>;
  onDraftsResponse?: (response: DraftsResponse<E>) => void;
  produceDraft: (req: {
    item_id: string;
    idempotency_key: string;
    actor_id: null;
  }) => Promise<object>;
  actionDraft: (
    draftId: string,
    req: {
      action: "approve" | "reject";
      expected_revision: number;
      idempotency_key: string;
      actor_id: null;
    },
  ) => Promise<MutationResponse>;
  /** Panel-specific 3-minute timeout message. */
  produceTimeoutText: string;
}

export interface UseDraftPanelReturn<E extends DraftEntry> {
  drafts: E[];
  loaded: boolean;
  active: E | undefined;
  producing: boolean;
  busy: boolean;
  notice: DraftPanelNotice;
  setNotice: React.Dispatch<React.SetStateAction<DraftPanelNotice>>;
  produce: () => Promise<void>;
  /** saveEdits is called only on approve, when provided. */
  runAction: (
    entry: E,
    action: "approve" | "reject",
    saveEdits?: (revision: number) => Promise<number>,
  ) => Promise<void>;
  load: () => Promise<void>;
}

export function useDraftPanel<E extends DraftEntry>(
  args: UseDraftPanelArgs<E>,
): UseDraftPanelReturn<E> {
  const {
    itemId,
    produceKind,
    onUnauthorized,
    fetchDrafts,
    produceDraft,
    actionDraft,
    produceTimeoutText,
  } = args;

  const [drafts, setDrafts] = useState<E[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [producing, setProducing] = useState(false);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<DraftPanelNotice>(null);
  const produceIdempotencyKeyRef = useRef<string | null>(null);

  // Panels pass inline closures for fetchDrafts; ride a ref so `load` keeps
  // the original panels' [itemId, onUnauthorized] identity — a fresh closure
  // per render must not re-trigger the load effect (infinite fetch loop).
  const fetchDraftsRef = useRef(fetchDrafts);
  fetchDraftsRef.current = fetchDrafts;
  const onDraftsResponseRef = useRef(args.onDraftsResponse);
  onDraftsResponseRef.current = args.onDraftsResponse;

  const load = useCallback(async () => {
    try {
      const res = await fetchDraftsRef.current(itemId);
      setDrafts(res.drafts);
      onDraftsResponseRef.current?.(res);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice({ text: `Failed to load drafts: ${errorMessage(err)}`, kind: "error" });
    } finally {
      setLoaded(true);
    }
  }, [itemId, onUnauthorized]);

  useEffect(() => {
    void load();
  }, [load]);

  const active = drafts.find((d) => d.draft.status !== "rejected");

  // Poll while a delivery is pending — naturally inert for entries without outbox_job.
  const deliveryPending = drafts.some(
    (entry) =>
      entry.draft.status === "approved" &&
      entry.outbox_job?.status === "pending",
  );
  useEffect(() => {
    if (!deliveryPending) return;
    const id = setInterval(() => void load(), 5_000);
    return () => clearInterval(id);
  }, [deliveryPending, load]);

  // A kicked-off produce runs in the background — poll until either the staged
  // draft lands or the produce status read model reports a failed stage receipt.
  const hasActive = active != null;
  useEffect(() => {
    if (!producing) return;
    if (hasActive) {
      setProducing(false);
      produceIdempotencyKeyRef.current = null;
      return;
    }
    let polls = 0;
    const id = setInterval(() => {
      polls += 1;
      if (polls > 36) {
        setProducing(false);
        produceIdempotencyKeyRef.current = null;
        setNotice({ text: produceTimeoutText, kind: "error" });
        return;
      }
      void (async () => {
        await load();
        const idempotencyKey = produceIdempotencyKeyRef.current;
        if (!idempotencyKey) return;
        const status = await api.produceStatus(itemId, produceKind, idempotencyKey);
        if (status.status === "failed") {
          setProducing(false);
          produceIdempotencyKeyRef.current = null;
          const detail = status.message?.trim();
          const message = errorCodeMessage(status.error_code) ?? "Something went wrong — please try again.";
          setNotice({
            text: `Produce failed: ${detail ? `${message} Reason: ${detail}` : message}`,
            kind: "error",
          });
        }
      })().catch((err) => {
        if (isUnauthorized(err)) onUnauthorized();
        else setNotice({ text: `Failed to check produce status: ${errorMessage(err)}`, kind: "error" });
      });
    }, 5_000);
    return () => clearInterval(id);
  }, [producing, hasActive, itemId, produceKind, load, onUnauthorized, produceTimeoutText]);

  const produce = async () => {
    setProducing(true);
    setNotice(null);
    const idempotencyKey = crypto.randomUUID();
    produceIdempotencyKeyRef.current = idempotencyKey;
    try {
      const res = await produceDraft({
        item_id: itemId,
        idempotency_key: idempotencyKey,
        actor_id: null,
      });
      await load();
      // A draft response means an active draft already existed; otherwise the
      // route returned a fast 202 and polling will resolve success/failure.
      if ("draft" in res) {
        setProducing(false);
        produceIdempotencyKeyRef.current = null;
      }
    } catch (err) {
      setProducing(false);
      produceIdempotencyKeyRef.current = null;
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice({ text: `Produce failed: ${errorMessage(err)}`, kind: "error" });
    }
  };

  const runAction = async (
    entry: E,
    action: "approve" | "reject",
    saveEdits?: (revision: number) => Promise<number>,
  ) => {
    setBusy(true);
    setNotice(null);
    try {
      let revision = entry.revision;
      if (action === "approve" && saveEdits) {
        // Approve-with-edits: persist the operator's changes (receipted),
        // then approve the edited draft in the same flow.
        revision = await saveEdits(revision);
      }
      await actionDraft(entry.draft.draft_id, {
        action,
        expected_revision: revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      await load();
    } catch (err) {
      if (isUnauthorized(err)) {
        onUnauthorized();
      } else if (isRevisionConflict(err)) {
        setNotice({ text: "Draft changed elsewhere — reloaded.", kind: "conflict" });
        await load();
      } else {
        setNotice({ text: `${action} failed: ${errorMessage(err)}`, kind: "error" });
      }
    } finally {
      setBusy(false);
    }
  };

  return { drafts, loaded, active, producing, busy, notice, setNotice, produce, runAction, load };
}
