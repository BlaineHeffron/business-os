import { useEffect, useRef, useState } from "react";
import type { ContentCollisionMatch } from "../types/generated/ContentCollisionMatch";
import type { ContentCollisionSummary } from "../types/generated/ContentCollisionSummary";
import type { ContentDraftWithRevision } from "../types/generated/ContentDraftWithRevision";
import {
  api,
  errorMessage,
  isRevisionConflict,
  isUnauthorized,
} from "../lib/api";
import { Button, StatusBadge } from "./ui";
import DraftFieldInput from "./DraftFieldInput";
import type { StatusTone } from "../lib/status";
import {
  useDraftPanel,
  useDraftEdit,
  DraftPanelShell,
  DraftEmptyCta,
  DraftStatusHeader,
  DraftActionFooter,
  OutboxStateLine,
} from "./draft";

type ContentEdit = {
  title: string;
  body_markdown: string;
  target_query: string;
  meta_description: string;
};

const REASON_LABEL: Record<string, string> = {
  exact_query: "Same target search",
  same_slug: "Same page or URL",
  similar: "Possible overlap",
};

function reasonLabel(match: ContentCollisionMatch): string {
  return REASON_LABEL[match.reason] ?? "Possible overlap";
}

function suggestedSlug(title: string): string {
  return title
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 120)
    .replace(/-+$/g, "");
}

function localCivilDate(date = new Date()): string {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

/** Detail panel under an accepted queue row for the content_draft kind:
 * produce a grounded draft from the brief (evidence comes from the Drive
 * corpus; every claim must cite snippets), review the citation gate, edit
 * the text fields, then approve. DRAFT-ONLY — approval marks the draft
 * deliverable; publishing stays manual, so the approved markdown is what
 * you copy out. Approve is blocked while the citation gate fails. */
export default function ContentDraftPanel({
  itemId,
  onUnauthorized,
}: {
  itemId: string;
  onUnauthorized: () => void;
}) {
  const [publishingAvailable, setPublishingAvailable] = useState(false);
  const [publishingLiveEnabled, setPublishingLiveEnabled] = useState(false);
  const {
    drafts,
    loaded,
    active,
    producing,
    busy,
    notice,
    setNotice,
    produce,
    runAction,
    load,
  } =
    useDraftPanel<ContentDraftWithRevision>({
      itemId,
      produceKind: "content_draft",
      onUnauthorized,
      fetchDrafts: (id) => api.contentDrafts(id),
      onDraftsResponse: (response) => {
        setPublishingAvailable(response.publishing_available ?? false);
        setPublishingLiveEnabled(response.publishing_live_enabled ?? false);
      },
      produceDraft: (req) => api.produceContentDraft(req),
      actionDraft: (draftId, req) => api.contentDraftAction(draftId, req),
      produceTimeoutText:
        "The draft didn't finish after 3 minutes — drafting may have failed (check AI Usage). Try again.",
    });
  const [publishSlug, setPublishSlug] = useState("");
  const [publishedAt, setPublishedAt] = useState(() => localCivilDate());
  const [publishing, setPublishing] = useState(false);
  const publishAttemptRef = useRef<{
    fingerprint: string;
    idempotencyKey: string;
  } | null>(null);

  const [edit, setEdit] = useDraftEdit<ContentDraftWithRevision, ContentEdit>(
    active,
    (entry) => ({
      title: entry.draft.title,
      body_markdown: entry.draft.body_markdown,
      target_query: entry.draft.target_query ?? "",
      meta_description: entry.draft.meta_description ?? "",
    }),
  );

  const dirty =
    active != null &&
    edit != null &&
    (edit.title !== active.draft.title ||
      edit.body_markdown !== active.draft.body_markdown ||
      edit.target_query !== (active.draft.target_query ?? "") ||
      edit.meta_description !== (active.draft.meta_description ?? ""));

  const [showEvidence, setShowEvidence] = useState(false);
  const [overlapSummary, setOverlapSummary] =
    useState<ContentCollisionSummary | null>(null);

  const gate = active?.draft.citation_gate;
  const activeDraftId = active?.draft.draft_id ?? null;
  const activeDraftStatus = active?.draft.status ?? null;

  useEffect(() => {
    if (!active || active.draft.status !== "approved") return;
    setPublishSlug(suggestedSlug(active.draft.title));
  }, [active?.draft.draft_id, active?.draft.status, active?.draft.title]);

  const publish = async () => {
    if (!active) return;
    const fingerprint = JSON.stringify([
      active.draft.draft_id,
      active.revision,
      publishSlug,
      publishedAt,
    ]);
    if (publishAttemptRef.current?.fingerprint !== fingerprint) {
      publishAttemptRef.current = {
        fingerprint,
        idempotencyKey: crypto.randomUUID(),
      };
    }
    const idempotencyKey = publishAttemptRef.current.idempotencyKey;
    setPublishing(true);
    setNotice(null);
    try {
      await api.publishContentDraft(active.draft.draft_id, {
        slug: publishSlug,
        published_at: publishedAt,
        expected_revision: active.revision,
        idempotency_key: idempotencyKey,
        actor_id: null,
      });
      publishAttemptRef.current = null;
      await load();
    } catch (err) {
      if (isUnauthorized(err)) {
        onUnauthorized();
      } else if (isRevisionConflict(err)) {
        publishAttemptRef.current = null;
        setNotice({ text: "Draft changed elsewhere — reloaded.", kind: "conflict" });
        await load();
      } else {
        setNotice({
          text: `Publish failed: ${errorMessage(err)}`,
          kind: "error",
        });
      }
    } finally {
      setPublishing(false);
    }
  };

  useEffect(() => {
    setOverlapSummary(null);
    if (!activeDraftId || activeDraftStatus !== "staged") return;
    let cancelled = false;
    void api
      .contentDraftOverlap(activeDraftId)
      .then((response) => {
        if (!cancelled) setOverlapSummary(response.summary);
      })
      .catch(() => {
        if (!cancelled) setOverlapSummary(null);
      });
    return () => {
      cancelled = true;
    };
  }, [activeDraftId, activeDraftStatus]);

  const claimStatusTone = (status: string): StatusTone =>
    status === "supported"
      ? "ok"
      : status === "missing_citation"
        ? "warning"
        : "critical";

  return (
    <DraftPanelShell loaded={loaded} notice={notice}>
      {!active ? (
        <DraftEmptyCta
          message="No content draft yet — create one from this item's brief. Your Drive documents will be used as the source."
          buttonLabel="Draft content"
          busyLabel="Drafting…"
          producing={producing}
          onProduce={() => void produce()}
          historyCount={drafts.length}
        />
      ) : (
        <div className="flex flex-col gap-2">
          <DraftStatusHeader
            status={active.draft.status}
            confidence={active.draft.confidence}
            model={active.draft.model}
          />

          {gate ? (
            gate.passed ? (
              <div className="rounded-md border border-emerald-900/60 bg-emerald-950/30 px-3 py-1.5 text-xs text-emerald-300">
                All statements are supported — every claim cites your source documents.
              </div>
            ) : (
              <div className="rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-1.5 text-xs text-amber-300">
                Some statements couldn&apos;t be verified ({gate.missing_citation_claim_ids.length} uncited,{" "}
                {gate.unsupported_claim_ids.length} unsupported) — approval is blocked. Reject and try again, or update your Drive documents.
              </div>
            )
          ) : null}

          {active.draft.status === "staged" &&
          overlapSummary?.matches.length ? (
            <div className="rounded-md border border-amber-900/60 bg-amber-950/25 px-3 py-2 text-xs text-amber-200">
              <div className="font-semibold text-amber-100">
                Possible overlap with existing content
              </div>
              <ul className="mt-1 flex flex-col gap-0.5">
                {overlapSummary.matches.slice(0, 3).map((match) => (
                  <li key={match.inventory_id}>
                    {reasonLabel(match)}:{" "}
                    <span className="text-amber-100">{match.title}</span>
                  </li>
                ))}
              </ul>
            </div>
          ) : null}

          {active.draft.status === "staged" && edit != null ? (
            <div className="flex max-w-2xl flex-col gap-2">
              <DraftFieldInput
                label="Title"
                value={edit.title}
                onChange={(title) => setEdit({ ...edit, title })}
                quote=""
                disabled={busy}
              />
              <DraftFieldInput
                label="Body (markdown)"
                value={edit.body_markdown}
                onChange={(body_markdown) => setEdit({ ...edit, body_markdown })}
                quote=""
                multiline
                disabled={busy}
              />
              <div className="grid grid-cols-2 gap-2">
                <DraftFieldInput
                  label="Target query"
                  value={edit.target_query}
                  onChange={(target_query) => setEdit({ ...edit, target_query })}
                  quote=""
                  disabled={busy}
                />
                <DraftFieldInput
                  label="Meta description"
                  value={edit.meta_description}
                  onChange={(meta_description) =>
                    setEdit({ ...edit, meta_description })
                  }
                  quote=""
                  disabled={busy}
                />
              </div>
            </div>
          ) : (
            <div className="flex flex-col gap-1 text-xs">
              <span className="font-semibold text-zinc-200">
                {active.draft.title}
              </span>
              <pre className="max-h-72 overflow-y-auto whitespace-pre-wrap rounded-md border border-zinc-800 bg-zinc-900/60 p-2 font-sans text-zinc-300">
                {active.draft.body_markdown}
              </pre>
            </div>
          )}

          {active.draft.claims.length > 0 ? (
            <div className="flex flex-col gap-1">
              <span className="text-xs font-semibold uppercase tracking-wide text-zinc-400">
                Claims ({active.draft.claims.length})
              </span>
              <ul className="flex flex-col gap-1">
                {active.draft.claims.map((claim) => (
                  <li key={claim.claim_id} className="flex items-start gap-2 text-xs">
                    <StatusBadge tone={claimStatusTone(claim.status)}>
                      {claim.status.replace("_", " ")}
                    </StatusBadge>
                    <span className="text-zinc-300">
                      {claim.text}
                      {claim.snippet_ids.length > 0 ? (
                        <span className="ml-1 text-xs text-zinc-500">
                          [{claim.snippet_ids.join(", ")}]
                        </span>
                      ) : null}
                    </span>
                  </li>
                ))}
              </ul>
            </div>
          ) : null}

          <div className="flex flex-col gap-1.5">
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setShowEvidence((s) => !s)}
              className="self-start"
            >
              {showEvidence ? "Hide" : "Show"} evidence (
              {active.draft.evidence.length} snippets)
            </Button>
            {showEvidence ? (
              <ul className="flex flex-col gap-1.5">
                {active.draft.evidence.map((snippet) => (
                  <li
                    key={snippet.snippet_id}
                    className="rounded-md border border-zinc-800 bg-zinc-900/40 px-2 py-1.5 text-xs"
                  >
                    <div className="text-zinc-400">
                      <span className="font-mono text-zinc-500">
                        [{snippet.snippet_id}]
                      </span>{" "}
                      {snippet.web_view_link ? (
                        <a
                          href={snippet.web_view_link}
                          target="_blank"
                          rel="noreferrer"
                          className="text-sky-400 hover:underline"
                        >
                          {snippet.doc_title}
                        </a>
                      ) : (
                        <span className="text-zinc-300">{snippet.doc_title}</span>
                      )}
                      {snippet.heading_path.length > 0
                        ? ` — ${snippet.heading_path.join(" > ")}`
                        : ""}
                    </div>
                    <div className="mt-0.5 line-clamp-3 whitespace-pre-wrap text-zinc-500">
                      {snippet.text}
                    </div>
                  </li>
                ))}
              </ul>
            ) : null}
          </div>

          <DraftActionFooter
            visible={active.draft.status === "staged"}
            busy={busy}
            dirty={dirty}
            approveLabel="Approve draft"
            approveDirtyLabel="Save & approve draft"
            approveTitle={
              gate?.passed
                ? "Mark this draft approved — it becomes the deliverable; publishing stays manual"
                : "Blocked: some statements are not supported by your documents"
            }
            approveDisabled={!gate?.passed}
            onApprove={() =>
              void runAction(
                active,
                "approve",
                dirty && edit
                  ? async (revision) => {
                      const saved = await api.updateContentDraft(
                        active.draft.draft_id,
                        {
                          title: edit.title,
                          body_markdown: edit.body_markdown,
                          target_query: edit.target_query.trim()
                            ? edit.target_query
                            : null,
                          meta_description: edit.meta_description.trim()
                            ? edit.meta_description
                            : null,
                          expected_revision: revision,
                          idempotency_key: crypto.randomUUID(),
                          actor_id: null,
                        },
                      );
                      return saved.revision ?? revision + 1;
                    }
                  : undefined,
              )
            }
            onReject={() => void runAction(active, "reject")}
            onResetEdits={() =>
              setEdit({
                title: active.draft.title,
                body_markdown: active.draft.body_markdown,
                target_query: active.draft.target_query ?? "",
                meta_description: active.draft.meta_description ?? "",
              })
            }
          />

          {active.draft.status === "approved" ? (
            <div className="flex flex-col gap-2 rounded-md border border-zinc-800 bg-zinc-900/30 p-2">
              <div className="flex items-center gap-2 text-xs">
                <StatusBadge tone="ok">approved</StatusBadge>
                <span className="text-zinc-400">
                  The draft is approved. Publishing is a separate action.
                </span>
                <Button
                  variant="secondary"
                  size="sm"
                  onClick={() =>
                    void navigator.clipboard.writeText(active.draft.body_markdown)
                  }
                >
                  Copy markdown
                </Button>
              </div>
              {publishingAvailable ? (
                <>
                  {!active.outbox_job ||
                  active.outbox_job.status === "failed_terminal" ||
                  (active.outbox_job.status === "delivered" &&
                    active.outbox_job.dry_run) ? (
                    <div className="flex flex-wrap items-end gap-2">
                      <DraftFieldInput
                        label="URL slug"
                        value={publishSlug}
                        onChange={setPublishSlug}
                        quote=""
                        disabled={publishing}
                      />
                      <label className="flex flex-col gap-1 text-xs text-zinc-400">
                        Publication date
                        <input
                          type="date"
                          value={publishedAt}
                          onChange={(event) => setPublishedAt(event.target.value)}
                          disabled={publishing}
                          className="rounded border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-zinc-200"
                        />
                      </label>
                      <Button
                        variant="primary"
                        size="sm"
                        busy={publishing}
                        disabled={
                          publishing ||
                          !publishSlug.trim() ||
                          !publishedAt.trim()
                        }
                        onClick={() => void publish()}
                      >
                        {publishing
                          ? "Publishing…"
                          : publishingLiveEnabled
                            ? "Publish post"
                            : "Validate publish"}
                      </Button>
                      {!publishingLiveEnabled ? (
                        <span className="text-xs text-amber-300">
                          Live publishing is off; this will dry-run.
                        </span>
                      ) : null}
                    </div>
                  ) : null}
                  <OutboxStateLine
                    job={active.outbox_job}
                    show={Boolean(active.outbox_job)}
                    dryRunText="Publish validation passed. Enable live publishing, then publish again."
                    deliveredText={(job) =>
                      job.provider_object_id
                        ? `Published: ${job.provider_object_id}`
                        : "Published successfully."
                    }
                    onUnauthorized={onUnauthorized}
                    onRetried={load}
                  />
                  {active.outbox_job?.status === "delivered" &&
                  !active.outbox_job.dry_run &&
                  active.outbox_job.provider_object_id ? (
                    <a
                      href={active.outbox_job.provider_object_id}
                      target="_blank"
                      rel="noreferrer"
                      className="self-start text-xs text-sky-400 hover:underline"
                    >
                      Open published post
                    </a>
                  ) : null}
                </>
              ) : (
                <span className="text-xs text-zinc-400">
                  Direct publishing is not configured for this client.
                </span>
              )}
            </div>
          ) : null}
        </div>
      )}
    </DraftPanelShell>
  );
}
