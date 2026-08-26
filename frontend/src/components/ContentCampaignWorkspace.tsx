import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import { api, errorMessage, isRevisionConflict, isUnauthorized } from "../lib/api";
import type { ContentCampaignWorkspaceResponse } from "../types/generated/ContentCampaignWorkspaceResponse";
import type { ContentCampaignLaunchMode } from "../types/generated/ContentCampaignLaunchMode";
import type { ContentCampaignPublicationStatus } from "../types/generated/ContentCampaignPublicationStatus";
import type { OutboxJobSummary } from "../types/generated/OutboxJobSummary";
import type { SocialProposalTargetInput } from "../types/generated/SocialProposalTargetInput";
import { Button, EmptyState, StatusBadge } from "./ui";
import OutboxStateLine from "./draft/OutboxStateLine";

type Notice = { kind: "error" | "conflict" | "success"; text: string } | null;

interface CampaignPlanLocalFormState {
  planItemId: string;
  selectedChannelIds: string[];
  expectedUrl: string;
  publishedAt: string;
  launchMode: ContentCampaignLaunchMode;
}

export type CampaignPlanLocalFormAction =
  | { type: "plan_changed"; planItemId: string; today: string }
  | {
      type: "patch";
      patch: Partial<Omit<CampaignPlanLocalFormState, "planItemId">>;
    }
  | {
      type: "social_loaded";
      channelIds: string[];
      canonicalUrl: string;
    }
  | { type: "social_cleared" };

function localCivilDate(date = new Date()): string {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

export function initialCampaignPlanLocalFormState(
  planItemId: string,
  today = localCivilDate(),
): CampaignPlanLocalFormState {
  return {
    planItemId,
    selectedChannelIds: [],
    expectedUrl: "",
    publishedAt: today,
    launchMode: "publish_now",
  };
}

export function campaignPlanLocalFormReducer(
  state: CampaignPlanLocalFormState,
  action: CampaignPlanLocalFormAction,
): CampaignPlanLocalFormState {
  if (action.type === "plan_changed") {
    return initialCampaignPlanLocalFormState(action.planItemId, action.today);
  }
  if (action.type === "social_loaded") {
    const available = new Set(action.channelIds);
    const selectedChannelIds = state.selectedChannelIds.filter((id) => available.has(id));
    return {
      ...state,
      selectedChannelIds:
        selectedChannelIds.length > 0
          ? selectedChannelIds
          : action.channelIds,
      expectedUrl: action.canonicalUrl,
    };
  }
  if (action.type === "social_cleared") {
    return { ...state, selectedChannelIds: [], expectedUrl: "" };
  }
  return { ...state, ...action.patch };
}

function slugFrom(value: string): string {
  try {
    const url = new URL(value);
    const parts = url.pathname.split("/").filter(Boolean);
    if (parts.length > 0) return parts[parts.length - 1];
  } catch {
    // Fall through to title normalization.
  }
  return value
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 120)
    .replace(/-+$/g, "");
}

function localScheduleValue(value: string | null): string {
  if (!value) return "";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "";
  const local = new Date(date.getTime() - date.getTimezoneOffset() * 60_000);
  return local.toISOString().slice(0, 16);
}

function scheduledAt(value: string): string | null {
  if (!value) return null;
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? null : date.toISOString();
}

function defaultScheduledAt(civilDate: string): string {
  const date = new Date(`${civilDate}T09:00:00`);
  return Number.isNaN(date.getTime()) ? "" : date.toISOString();
}

function publicationLabel(status: string): string {
  if (status === "awaiting_blog") return "Publishing blog";
  if (status === "blog_dry_run") return "Blog dry-run complete";
  if (status === "social_enqueued") return "Publishing social posts";
  if (status === "completed") return "Campaign published";
  return "Review required";
}

function articleStatusLabel(status: string): string {
  if (status === "approved") return "Article approved";
  if (status === "staged") return "Article ready for review";
  return "Article rejected";
}

export function browserTimeZoneLabel(): string {
  const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  return zone ? `Your time · ${zone}` : "Your local time";
}

export function campaignRecoveryGuidance(
  status: ContentCampaignPublicationStatus,
  reviewReason: string | null | undefined,
  jobs: readonly OutboxJobSummary[],
): string | null {
  if (jobs.some((job) => job.status === "delivery_outcome_unknown")) {
    return "A social provider may have accepted a post. Check that destination in Buffer before doing anything else; BusinessOS will not retry an uncertain create.";
  }
  if (jobs.some((job) => job.status === "failed_terminal")) {
    return "Delivered destinations stay complete. Fix the failed Buffer connection or payload, then retry only that destination.";
  }
  if (status === "requires_review" && reviewReason?.includes("canonical")) {
    return "No social posts were created. Verify the live blog URL and publishing-connection result, then ask an administrator to reconcile this approved campaign snapshot; it cannot be edited in place.";
  }
  if (status === "requires_review") {
    return "Dependent publishing stopped. Review the blog and channel results below before retrying any failed destination.";
  }
  if (status === "blog_dry_run") {
    return "Live blog publishing is turned off, so this approval ran as a validation only — no live blog post and no social posts were created.";
  }
  return null;
}

function reviewReasonLabel(reason: string): string {
  const labels: Record<string, string> = {
    canonical_url_mismatch: "The publisher returned a different canonical URL",
    canonical_url_missing: "The publisher did not return a canonical URL",
    canonical_url_invalid: "The publisher returned an invalid canonical URL",
    social_delivery_failed: "One or more social destinations failed",
    delivery_outcome_unknown: "A social delivery outcome is unknown",
  };
  return labels[reason] ?? reason.replaceAll("_", " ");
}

export default function ContentCampaignWorkspace({
  planItemId,
  onUnauthorized,
  onPlanChanged,
}: {
  planItemId: string;
  onUnauthorized: () => void;
  onPlanChanged: () => Promise<void>;
}) {
  const [workspace, setWorkspace] = useState<ContentCampaignWorkspaceResponse | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [notice, setNotice] = useState<Notice>(null);
  const [articleEdit, setArticleEdit] = useState({
    title: "",
    body: "",
    targetQuery: "",
    metaDescription: "",
  });
  const [socialTargets, setSocialTargets] = useState<SocialProposalTargetInput[]>([]);
  const [planLocalForm, dispatchPlanLocalForm] = useReducer(
    campaignPlanLocalFormReducer,
    planItemId,
    initialCampaignPlanLocalFormState,
  );
  const loadRequestId = useRef(0);
  const currentPlanLocalForm =
    planLocalForm.planItemId === planItemId
      ? planLocalForm
      : initialCampaignPlanLocalFormState(planItemId);
  const {
    selectedChannelIds,
    expectedUrl,
    publishedAt,
    launchMode,
  } = currentPlanLocalForm;

  const load = useCallback(async () => {
    const requestId = ++loadRequestId.current;
    try {
      const next = await api.contentCampaignWorkspace(planItemId);
      if (loadRequestId.current !== requestId) return;
      setWorkspace(next);
      setNotice((current) => (current?.kind === "error" ? null : current));
    } catch (err) {
      if (loadRequestId.current !== requestId) return;
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice({ kind: "error", text: errorMessage(err) });
    } finally {
      if (loadRequestId.current === requestId) setLoaded(true);
    }
  }, [onUnauthorized, planItemId]);

  useEffect(() => {
    loadRequestId.current += 1;
    setWorkspace(null);
    setLoaded(false);
    setBusy(null);
    setNotice(null);
    setArticleEdit({ title: "", body: "", targetQuery: "", metaDescription: "" });
    setSocialTargets([]);
    dispatchPlanLocalForm({
      type: "plan_changed",
      planItemId,
      today: localCivilDate(),
    });
  }, [planItemId]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (!busy || !["generate-article", "generate-social", "publish"].includes(busy)) return;
    const timer = window.setInterval(() => void load(), 2_000);
    const timeout = window.setTimeout(() => {
      if (busy !== "publish") {
        setBusy(null);
        setNotice({
          kind: "error",
          text: "Generation did not finish after 3 minutes. Check AI Usage, then try again.",
        });
      }
    }, 180_000);
    return () => {
      window.clearInterval(timer);
      window.clearTimeout(timeout);
    };
  }, [busy, load]);

  const article = workspace?.content_draft ?? null;
  const social = workspace?.social_proposal ?? null;

  useEffect(() => {
    if (busy === "generate-article" && article) setBusy(null);
    if (busy === "generate-social" && social) setBusy(null);
    if (
      busy === "generate-social" &&
      workspace?.social_generation_status === "generation_failed"
    ) {
      setBusy(null);
      setNotice({
        kind: "error",
        text: workspace.social_generation_error
          ? workspace.social_generation_error.replaceAll("_", " ")
          : "Social variants could not be generated. Try again.",
      });
    }
    if (busy === "publish" && workspace?.publications.length) setBusy(null);
  }, [article, busy, social, workspace?.publications.length, workspace?.social_generation_error, workspace?.social_generation_status]);

  useEffect(() => {
    if (!article) return;
    setArticleEdit({
      title: article.draft.title,
      body: article.draft.body_markdown,
      targetQuery: article.draft.target_query ?? "",
      metaDescription: article.draft.meta_description ?? "",
    });
  }, [article?.draft.draft_id, article?.revision]);

  useEffect(() => {
    if (!social) {
      setSocialTargets([]);
      dispatchPlanLocalForm({ type: "social_cleared" });
      return;
    }
    setSocialTargets(
      social.proposal.targets.map((target) => ({
        channel_id: target.channel_id,
        text: target.text,
        image_url: target.image_url ?? null,
        utm: target.utm,
        schedule_mode: target.schedule_mode,
        due_at: target.due_at ?? null,
      })),
    );
    dispatchPlanLocalForm({
      type: "social_loaded",
      channelIds: social.proposal.targets.map((target) => target.channel_id),
      canonicalUrl: social.proposal.canonical_url,
    });
  }, [social?.proposal.proposal_id, social?.revision]);

  const articleDirty = Boolean(
    article &&
      (articleEdit.title !== article.draft.title ||
        articleEdit.body !== article.draft.body_markdown ||
        articleEdit.targetQuery !== (article.draft.target_query ?? "") ||
        articleEdit.metaDescription !== (article.draft.meta_description ?? "")),
  );
  const socialDirty = Boolean(
    social &&
      JSON.stringify(socialTargets) !==
        JSON.stringify(
          social.proposal.targets.map((target) => ({
            channel_id: target.channel_id,
            text: target.text,
            image_url: target.image_url ?? null,
            utm: target.utm,
            schedule_mode: target.schedule_mode,
            due_at: target.due_at ?? null,
          })),
        ),
  );
  const activePublication = workspace?.publications[0]?.publication ?? null;
  const recoveryGuidance = activePublication
    ? campaignRecoveryGuidance(
        activePublication.status,
        activePublication.review_reason,
        activePublication.social_outbox_jobs,
      )
    : null;
  const locked = Boolean(
    activePublication &&
      ["awaiting_blog", "social_enqueued", "completed", "requires_review"].includes(
        activePublication.status,
      ),
  );
  const selectedSocialReady =
    selectedChannelIds.length === 0 ||
    (social?.proposal.status === "staged" && !socialDirty);

  const actionError = async (err: unknown) => {
    if (isUnauthorized(err)) {
      onUnauthorized();
      return;
    }
    if (isRevisionConflict(err)) {
      setNotice({ kind: "conflict", text: "Changed elsewhere — reloaded." });
      await load();
      return;
    }
    setNotice({ kind: "error", text: errorMessage(err) });
  };

  const generateArticle = async () => {
    if (!workspace) return;
    setBusy("generate-article");
    setNotice(null);
    try {
      await api.generateContentCampaign(planItemId, {
        expected_revision: workspace.plan.revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNotice({ kind: "success", text: "Research and article generation started." });
      await load();
      await onPlanChanged();
    } catch (err) {
      await actionError(err);
      setBusy(null);
    }
  };

  const saveArticle = async (approve: boolean) => {
    if (!article) return;
    setBusy(approve ? "approve-article" : "save-article");
    setNotice(null);
    try {
      let revision = article.revision;
      if (articleDirty) {
        const result = await api.updateContentDraft(article.draft.draft_id, {
          title: articleEdit.title,
          body_markdown: articleEdit.body,
          target_query: articleEdit.targetQuery.trim() || null,
          meta_description: articleEdit.metaDescription.trim() || null,
          expected_revision: revision,
          idempotency_key: crypto.randomUUID(),
          actor_id: null,
        });
        revision = result.revision ?? revision + 1;
      }
      if (approve) {
        await api.contentDraftAction(article.draft.draft_id, {
          action: "approve",
          expected_revision: revision,
          idempotency_key: crypto.randomUUID(),
          actor_id: null,
        });
      }
      setNotice({
        kind: "success",
        text: approve ? "Exact article revision approved." : "Article staged.",
      });
      await load();
    } catch (err) {
      await actionError(err);
    } finally {
      setBusy(null);
    }
  };

  const generateSocial = async () => {
    if (!article || !expectedUrl.trim()) return;
    setBusy("generate-social");
    setNotice(null);
    try {
      await api.generateSocialDraftPreview(article.draft.draft_id, {
        expected_content_draft_revision: article.revision,
        expected_canonical_url: expectedUrl,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNotice({ kind: "success", text: "Social variants generation started." });
      await load();
    } catch (err) {
      await actionError(err);
      setBusy(null);
    }
  };

  const saveSocial = async () => {
    if (!social) return;
    setBusy("save-social");
    setNotice(null);
    try {
      await api.updateSocialProposal(social.proposal.proposal_id, {
        canonical_url: expectedUrl,
        targets: socialTargets,
        expected_revision: social.revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNotice({ kind: "success", text: "Social variants staged." });
      await load();
    } catch (err) {
      await actionError(err);
    } finally {
      setBusy(null);
    }
  };

  const discardSocial = async () => {
    if (!social) return;
    setBusy("discard-social");
    setNotice(null);
    try {
      await api.actionSocialProposal(social.proposal.proposal_id, {
        action: "reject",
        expected_revision: social.revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNotice({
        kind: "success",
        text: "Social stage discarded. Change the expected URL or generate again.",
      });
      await load();
    } catch (err) {
      await actionError(err);
    } finally {
      setBusy(null);
    }
  };

  const publish = async () => {
    if (!workspace || !article || !expectedUrl.trim()) return;
    setBusy("publish");
    setNotice(null);
    try {
      await api.publishContentCampaign(planItemId, {
        content_draft_id: article.draft.draft_id,
        expected_content_draft_revision: article.revision,
        social_proposal_id: selectedChannelIds.length > 0 ? social?.proposal.proposal_id ?? null : null,
        expected_social_proposal_revision:
          selectedChannelIds.length > 0 ? social?.revision ?? null : null,
        selected_channel_ids: selectedChannelIds,
        slug: slugFrom(expectedUrl),
        published_at: publishedAt,
        expected_canonical_url: expectedUrl,
        launch_mode: launchMode,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNotice({
        kind: "success",
        text: "Exact campaign revision approved. Blog queued first; social waits for its canonical URL.",
      });
      await load();
    } catch (err) {
      await actionError(err);
      setBusy(null);
    }
  };

  const canPublish = Boolean(
    article?.draft.status === "approved" &&
      article.draft.citation_gate.passed &&
      expectedUrl.trim() &&
      workspace?.blog_publishing_available &&
      selectedSocialReady &&
      !locked,
  );
  const publishBlockedReason = !article?.draft.citation_gate.passed
    ? "Resolve citation issues before approval."
    : article?.draft.status !== "approved"
      ? "Approve the current article revision first."
      : !expectedUrl.trim()
        ? "Enter the expected canonical blog URL."
        : !workspace?.blog_publishing_available
          ? "A blog publisher adapter must be configured."
          : selectedChannelIds.length > 0 && !social
            ? "Generate social variants for the selected destinations."
            : socialDirty && selectedChannelIds.length > 0
              ? "Save social changes before approving the campaign."
              : null;

  if (!loaded) return <div className="text-sm text-zinc-400">Loading campaign…</div>;
  if (!workspace) return <EmptyState title="Campaign unavailable">Reload this topic and try again.</EmptyState>;

  return (
    <div className="grid gap-4">
      {notice ? (
        <div className={`rounded-md border px-3 py-2 text-sm ${noticeClass(notice.kind)}`}>
          {notice.text}
        </div>
      ) : null}

      <div className="rounded-lg border border-zinc-800 bg-zinc-900/40 p-4">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div className="min-w-0 flex-1">
            <div className="text-xs uppercase tracking-wide text-zinc-400">Campaign workspace</div>
            <h3 className="mt-1 text-sm font-semibold text-zinc-200">
              Review each step, then approve one exact campaign snapshot
            </h3>
            <p className="mt-1 text-xs text-zinc-400">
              Social delivery is held until the blog adapter returns the exact URL previewed here.
            </p>
            <div className="mt-3 flex flex-wrap gap-2">
              <StatusBadge tone={workspace.blog_live_enabled ? "ok" : "warning"}>
                {workspace.blog_live_enabled ? "Blog writes live" : "Blog dry run"}
              </StatusBadge>
              <StatusBadge tone={!workspace.social_configured ? "neutral" : workspace.social_live_enabled ? "ok" : "warning"}>
                {!workspace.social_configured
                  ? "Social not configured"
                  : workspace.social_live_enabled
                    ? "Buffer writes live"
                    : "Buffer dry run"}
              </StatusBadge>
            </div>
          </div>
          {!article ? (
            <Button variant="primary" busy={busy === "generate-article"} onClick={() => void generateArticle()}>
              {busy === "generate-article" ? "Researching…" : "Generate campaign"}
            </Button>
          ) : null}
        </div>
        <ol className="mt-4 grid gap-2 sm:grid-cols-2" aria-label="Campaign progress">
          <CampaignStep
            number={1}
            label="Research & article"
            state={article?.draft.status === "approved" ? "complete" : "current"}
          />
          <CampaignStep
            number={2}
            label="Canonical URL"
            state={expectedUrl.trim() ? "complete" : article?.draft.status === "approved" ? "current" : "waiting"}
          />
          <CampaignStep
            number={3}
            label={workspace.social_configured ? "Social variants" : "Social optional"}
            state={!workspace.social_configured || social ? "complete" : expectedUrl.trim() ? "current" : "waiting"}
          />
          <CampaignStep
            number={4}
            label="Approval & delivery"
            state={activePublication ? (activePublication.status === "completed" ? "complete" : "current") : social || !workspace.social_configured ? "current" : "waiting"}
          />
        </ol>
      </div>

      {article ? (
        <section className="rounded-lg border border-zinc-800 bg-zinc-900/40 p-4">
          <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
            <div className="flex items-center gap-2">
              <h3 className="text-sm font-semibold text-zinc-200">Blog article</h3>
              <StatusBadge tone={article.draft.status === "approved" ? "ok" : "info"}>
                {articleStatusLabel(article.draft.status)}
              </StatusBadge>
              <StatusBadge tone={article.draft.citation_gate.passed ? "ok" : "critical"}>
                {article.draft.citation_gate.passed ? "Evidence checked" : "Evidence issues"}
              </StatusBadge>
            </div>
            {article.draft.status === "staged" ? (
              <div className="flex gap-2">
                <Button variant="secondary" busy={busy === "save-article"} onClick={() => void saveArticle(false)}>
                  {busy === "save-article" ? "Saving…" : "Save article"}
                </Button>
                <Button
                  variant="primary"
                  busy={busy === "approve-article"}
                  disabled={!article.draft.citation_gate.passed}
                  onClick={() => void saveArticle(true)}
                >
                  Approve article
                </Button>
              </div>
            ) : null}
          </div>
          {article.draft.status === "staged" ? (
            <div className="grid gap-3">
              <Field label="Title" value={articleEdit.title} onChange={(title) => setArticleEdit({ ...articleEdit, title })} />
              <Field label="Article markdown" value={articleEdit.body} multiline onChange={(body) => setArticleEdit({ ...articleEdit, body })} />
              <div className="grid gap-3 md:grid-cols-2">
                <Field label="Target keyword" value={articleEdit.targetQuery} onChange={(targetQuery) => setArticleEdit({ ...articleEdit, targetQuery })} />
                <Field label="Meta description" value={articleEdit.metaDescription} onChange={(metaDescription) => setArticleEdit({ ...articleEdit, metaDescription })} />
              </div>
            </div>
          ) : (
            <div className="grid gap-2">
              <div className="text-sm font-medium text-zinc-100">{article.draft.title}</div>
              <pre className="max-h-96 overflow-y-auto whitespace-pre-wrap rounded-md border border-zinc-800 bg-zinc-950 p-3 font-sans text-sm text-zinc-300">
                {article.draft.body_markdown}
              </pre>
            </div>
          )}
          <details className="mt-3 text-xs text-zinc-400">
            <summary className="cursor-pointer">Research evidence ({article.draft.evidence.length} cited snippets)</summary>
            <div className="mt-2 grid gap-2">
              {article.draft.evidence.map((snippet) => (
                <div key={snippet.snippet_id} className="rounded border border-zinc-800 bg-zinc-950 p-3">
                  <div className="font-medium text-zinc-300">[{snippet.snippet_id}] {snippet.doc_title}</div>
                  {snippet.heading_path.length > 0 ? (
                    <div className="mt-1 text-zinc-400">{snippet.heading_path.join(" → ")}</div>
                  ) : null}
                  <div className="mt-2 whitespace-pre-wrap text-zinc-300">{snippet.text}</div>
                  {snippet.web_view_link ? (
                    <a className="mt-2 inline-block text-sky-300 hover:underline" href={snippet.web_view_link} target="_blank" rel="noreferrer">
                      Open source document
                    </a>
                  ) : null}
                </div>
              ))}
            </div>
          </details>
        </section>
      ) : null}

      {article?.draft.status === "approved" ? (
        <section className="rounded-lg border border-zinc-800 bg-zinc-900/40 p-4">
          <div className="mb-3 flex flex-col items-stretch justify-between gap-3 sm:flex-row sm:items-end">
            <div className="min-w-0 flex-1">
              <label htmlFor="campaign-canonical-url" className="mb-1 block text-xs font-medium text-zinc-400">
                Expected canonical blog URL
              </label>
              <input
                id="campaign-canonical-url"
                type="url"
                aria-describedby="campaign-canonical-help"
                className="w-full rounded-md border border-zinc-700 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 outline-none focus-visible:border-sky-600 focus-visible:ring-2 focus-visible:ring-sky-500/30"
                value={expectedUrl}
                disabled={Boolean(social) || locked}
                onChange={(event) => dispatchPlanLocalForm({ type: "patch", patch: { expectedUrl: event.target.value } })}
                placeholder="https://example.com/blog/article-slug"
              />
              <p id="campaign-canonical-help" className="mt-1 text-xs text-zinc-400">
                Social posts stay blocked unless the blog publisher returns this exact HTTPS URL. Discard existing variants before changing it.
              </p>
            </div>
            {!social && workspace.social_configured ? (
              <Button
                variant="primary"
                busy={busy === "generate-social"}
                disabled={!expectedUrl.trim() || locked}
                onClick={() => void generateSocial()}
              >
                {busy === "generate-social" ? "Drafting…" : "Generate social variants"}
              </Button>
            ) : null}
          </div>
          {!workspace.social_configured ? (
            <p className="text-xs text-zinc-400">No social channels configured. Blog-only publishing remains available.</p>
          ) : null}
          {social ? (
            <div className="grid gap-3">
              <div className="flex flex-col items-start justify-between gap-2 sm:flex-row sm:items-center">
                <h3 className="text-sm font-semibold text-zinc-200">Social destinations</h3>
                <div className="flex flex-wrap gap-2">
                  <Button variant="secondary" busy={busy === "discard-social"} disabled={locked} onClick={() => void discardSocial()}>
                    {busy === "discard-social" ? "Discarding…" : "Discard variants"}
                  </Button>
                  <Button variant="secondary" busy={busy === "save-social"} disabled={!socialDirty || locked} onClick={() => void saveSocial()}>
                    {busy === "save-social" ? "Saving…" : "Save social variants"}
                  </Button>
                </div>
              </div>
              <p className="text-xs text-zinc-400">
                Select the destinations included in this approval. Clear every checkbox for a blog-only launch.
              </p>
              {socialTargets.map((target, index) => {
                const channel = workspace.channels.find((entry) => entry.channel_id === target.channel_id);
                const checked = selectedChannelIds.includes(target.channel_id);
                return (
                  <div key={target.channel_id} className="rounded-md border border-zinc-800 bg-zinc-950 p-3">
                    <div className="mb-2 flex items-center justify-between gap-2">
                      <label className="flex items-center gap-2 text-sm font-medium text-zinc-200">
                        <input
                          type="checkbox"
                          checked={checked}
                          disabled={locked}
                          onChange={() =>
                            dispatchPlanLocalForm({
                              type: "patch",
                              patch: {
                                selectedChannelIds: checked
                                  ? selectedChannelIds.filter((id) => id !== target.channel_id)
                                  : [...selectedChannelIds, target.channel_id],
                              },
                            })
                          }
                        />
                        {channel?.name ?? target.channel_id}
                      </label>
                      <StatusBadge tone="neutral">{channel?.platform ?? "social"}</StatusBadge>
                    </div>
                    <label className="block text-xs font-medium text-zinc-400">
                      Post text
                    <textarea
                      aria-label={`Post text for ${channel?.name ?? target.channel_id}`}
                      className="min-h-24 w-full rounded-md border border-zinc-700 bg-zinc-900 px-3 py-2 text-sm text-zinc-100 outline-none focus-visible:border-sky-600 focus-visible:ring-2 focus-visible:ring-sky-500/30 disabled:text-zinc-400"
                      value={target.text}
                      disabled={locked}
                      onChange={(event) =>
                        setSocialTargets((current) =>
                          current.map((entry, entryIndex) =>
                            entryIndex === index ? { ...entry, text: event.target.value } : entry,
                          ),
                        )
                      }
                    />
                    </label>
                    <div className="mt-1 text-right text-xs tabular-nums text-zinc-400">
                      {target.text.length.toLocaleString()} characters
                    </div>
                    <div className="mt-2 flex flex-wrap items-end gap-2">
                      <label className="flex flex-col gap-1 text-xs text-zinc-400">
                        Buffer timing
                        <select
                          className="rounded border border-zinc-700 bg-zinc-900 px-2 py-1.5 text-sm text-zinc-200 outline-none focus-visible:border-sky-600 focus-visible:ring-2 focus-visible:ring-sky-500/30"
                          value={target.schedule_mode}
                          disabled={locked}
                          onChange={(event) => {
                            const scheduleMode = event.target
                              .value as SocialProposalTargetInput["schedule_mode"];
                            setSocialTargets((current) =>
                              current.map((entry, entryIndex) =>
                                entryIndex === index
                                  ? {
                                      ...entry,
                                      schedule_mode: scheduleMode,
                                      due_at:
                                        scheduleMode === "scheduled"
                                          ? entry.due_at ?? defaultScheduledAt(publishedAt)
                                          : null,
                                    }
                                  : entry,
                              ),
                            );
                          }}
                        >
                          <option value="queue">Add to queue</option>
                          <option value="scheduled">Schedule exact time</option>
                        </select>
                      </label>
                      {target.schedule_mode === "scheduled" ? (
                        <label className="flex min-w-0 flex-col gap-1 text-xs text-zinc-400">
                          Exact publish time
                          <input
                            type="datetime-local"
                            className="rounded border border-zinc-700 bg-zinc-900 px-2 py-1.5 text-sm text-zinc-200 outline-none focus-visible:border-sky-600 focus-visible:ring-2 focus-visible:ring-sky-500/30"
                            value={localScheduleValue(target.due_at ?? null)}
                            disabled={locked}
                            onChange={(event) =>
                              setSocialTargets((current) =>
                                current.map((entry, entryIndex) =>
                                  entryIndex === index
                                    ? { ...entry, due_at: scheduledAt(event.target.value) }
                                    : entry,
                                ),
                              )
                            }
                          />
                          <span>{browserTimeZoneLabel()}</span>
                        </label>
                      ) : null}
                    </div>
                  </div>
                );
              })}
            </div>
          ) : null}
        </section>
      ) : null}

      {article?.draft.status === "approved" ? (
        <section className="rounded-lg border border-zinc-800 bg-zinc-900/40 p-4">
          <div className="mb-3 flex items-center justify-between gap-3">
            <div>
              <h3 className="text-sm font-semibold text-zinc-200">Approval and launch</h3>
              <p className="mt-1 text-xs text-zinc-400">Blog adapter plus {selectedChannelIds.length} selected social destination{selectedChannelIds.length === 1 ? "" : "s"}.</p>
            </div>
            {activePublication ? (
              <StatusBadge tone={activePublication.status === "completed" ? "ok" : activePublication.status === "requires_review" ? "critical" : "progress"}>
                {publicationLabel(activePublication.status)}
              </StatusBadge>
            ) : null}
          </div>
          {!locked ? (
            <div className="grid gap-3 sm:grid-cols-2 sm:items-end">
              <label className="flex flex-col gap-1 text-xs text-zinc-400">
                Launch mode
                <select className="rounded border border-zinc-700 bg-zinc-950 px-2 py-2 text-sm text-zinc-200 outline-none focus-visible:border-sky-600 focus-visible:ring-2 focus-visible:ring-sky-500/30" value={launchMode} onChange={(event) => dispatchPlanLocalForm({ type: "patch", patch: { launchMode: event.target.value as ContentCampaignLaunchMode } })}>
                  <option value="publish_now">Publish now</option>
                  <option value="schedule">Schedule blog date</option>
                </select>
              </label>
              {launchMode === "schedule" ? (
                <label className="flex flex-col gap-1 text-xs text-zinc-400">
                  Blog publication date
                  <input type="date" className="rounded border border-zinc-700 bg-zinc-950 px-2 py-2 text-sm text-zinc-200 outline-none focus-visible:border-sky-600 focus-visible:ring-2 focus-visible:ring-sky-500/30" value={publishedAt} onChange={(event) => dispatchPlanLocalForm({ type: "patch", patch: { publishedAt: event.target.value } })} />
                  <span>Date only; the blog adapter controls its publish time and time zone.</span>
                </label>
              ) : (
                <div className="rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2 text-xs text-zinc-400">
                  Queues blog delivery as soon as approval is accepted. Social still waits for the exact blog URL.
                </div>
              )}
              <div className="flex flex-col items-start gap-2 sm:col-span-2 sm:flex-row sm:items-center sm:justify-between">
                {publishBlockedReason ? (
                  <span className="min-w-0 flex-1 text-xs text-zinc-400">{publishBlockedReason}</span>
                ) : !workspace.blog_live_enabled ? (
                  <span className="min-w-0 flex-1 text-xs text-amber-300">
                    Validation only — no live blog post or social post will be created.
                  </span>
                ) : (
                  <span className="min-w-0 flex-1 text-xs text-zinc-400">This approves the exact article, URL, destinations, and timing shown above.</span>
                )}
                <Button className="flex-none" variant="primary" busy={busy === "publish"} disabled={!canPublish} onClick={() => void publish()}>
                  {busy === "publish"
                    ? "Approving…"
                    : launchMode === "publish_now"
                      ? "Approve & publish now"
                      : "Approve & schedule"}
                </Button>
              </div>
            </div>
          ) : null}
          {activePublication ? (
            <div className="mt-3 grid gap-2">
              {activePublication.review_reason ? (
                <div className="rounded border border-red-900/60 bg-red-950/30 px-3 py-2 text-xs text-red-200">
                  <div className="font-semibold">Social publishing stopped</div>
                  <div className="mt-1">
                    {reviewReasonLabel(activePublication.review_reason)}. Expected{" "}
                    <span className="break-all font-medium">{activePublication.expected_canonical_url}</span>; received{" "}
                    <span className="break-all font-medium">{activePublication.actual_canonical_url ?? "no canonical URL"}</span>.
                  </div>
                </div>
              ) : null}
              {recoveryGuidance ? (
                <div className="rounded border border-amber-900/60 bg-amber-950/30 px-3 py-2 text-xs text-amber-200">
                  <span className="font-semibold">Next step:</span> {recoveryGuidance}
                </div>
              ) : null}
              <OutboxStateLine job={activePublication.blog_outbox_job} show dryRunText="Blog validation passed. No social posts were created." deliveredText={(job) => job.provider_object_id ? `Blog published: ${job.provider_object_id}` : "Blog published."} onUnauthorized={onUnauthorized} onRetried={load} />
              {activePublication.social_outbox_jobs.map((job) => (
                <OutboxStateLine key={job.job_id} job={job} show dryRunText="Social destination validated in dry-run." deliveredText={() => "Social destination delivered."} onUnauthorized={onUnauthorized} onRetried={load} />
              ))}
            </div>
          ) : null}
        </section>
      ) : null}
    </div>
  );
}

function CampaignStep({
  number,
  label,
  state,
}: {
  number: number;
  label: string;
  state: "complete" | "current" | "waiting";
}) {
  const stateLabel = state === "complete" ? "Complete" : state === "current" ? "Current" : "Waiting";
  return (
    <li
      aria-current={state === "current" ? "step" : undefined}
      className={`flex min-w-0 items-center gap-2 rounded-md border px-3 py-2 text-xs ${
        state === "complete"
          ? "border-emerald-800/60 bg-emerald-950/20 text-emerald-200"
          : state === "current"
            ? "border-sky-800/60 bg-sky-950/20 text-sky-200"
            : "border-zinc-800 bg-zinc-950 text-zinc-400"
      }`}
    >
      <span className="flex h-5 w-5 flex-none items-center justify-center rounded-full border border-current font-semibold tabular-nums">
        {number}
      </span>
      <span className="min-w-0 flex-1 font-medium">{label}</span>
      <span className="text-zinc-400">{stateLabel}</span>
    </li>
  );
}

function Field({
  label,
  value,
  onChange,
  multiline = false,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  multiline?: boolean;
}) {
  const cls = "w-full rounded-md border border-zinc-700 bg-zinc-950 px-3 py-2 text-sm text-zinc-100 outline-none focus-visible:border-sky-600 focus-visible:ring-2 focus-visible:ring-sky-500/30";
  return (
    <label>
      <span className="mb-1 block text-xs font-medium text-zinc-400">{label}</span>
      {multiline ? (
        <textarea className={`${cls} min-h-72 font-mono`} value={value} onChange={(event) => onChange(event.target.value)} />
      ) : (
        <input className={cls} value={value} onChange={(event) => onChange(event.target.value)} />
      )}
    </label>
  );
}

function noticeClass(kind: NonNullable<Notice>["kind"]): string {
  if (kind === "success") return "border-emerald-900/60 bg-emerald-950/30 text-emerald-200";
  if (kind === "conflict") return "border-amber-900/60 bg-amber-950/30 text-amber-200";
  return "border-red-900/60 bg-red-950/30 text-red-200";
}
