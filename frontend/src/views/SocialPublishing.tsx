import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api, errorMessage, isRevisionConflict, isUnauthorized } from "../lib/api";
import OutboxStateLine from "../components/draft/OutboxStateLine";
import SectionHelpButton from "../components/SectionHelpButton";
import { Button, EmptyState, SkeletonList, StatusBadge, Surface } from "../components/ui";
import type { SocialPostProposalWithRevision } from "../types/generated/SocialPostProposalWithRevision";
import type { SocialProposalStatus } from "../types/generated/SocialProposalStatus";
import type { SocialProposalTargetInput } from "../types/generated/SocialProposalTargetInput";
import type { SocialPublishedSource } from "../types/generated/SocialPublishedSource";
import type { SocialPublishingChannel } from "../types/generated/SocialPublishingChannel";

type Notice = { kind: "success" | "error" | "conflict"; text: string } | null;

function statusTone(status: SocialProposalStatus): "warning" | "ok" | "neutral" {
  if (status === "approved") return "ok";
  if (status === "rejected") return "neutral";
  return "warning";
}

function statusLabel(status: SocialProposalStatus): string {
  if (status === "approved") return "Approved";
  if (status === "rejected") return "Rejected";
  return "Needs approval";
}

function targetInput(
  channel: SocialPublishingChannel,
  source?: SocialPublishedSource,
): SocialProposalTargetInput {
  const text = source ? `${source.title}\n\n${source.canonical_url}` : "";
  return {
    channel_id: channel.channel_id,
    text,
    image_url: null,
    utm: {
      source: channel.platform,
      medium: "social",
      campaign: "blog",
      content: null,
    },
    schedule_mode: "queue",
    due_at: null,
  };
}

function localDateTimeValue(value: string | null | undefined): string {
  if (!value) return "";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "";
  const local = new Date(date.getTime() - date.getTimezoneOffset() * 60_000);
  return local.toISOString().slice(0, 16);
}

function rfc3339Value(value: string): string | null {
  if (!value) return null;
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? null : date.toISOString();
}

export function targetRequest(target: SocialProposalTargetInput): SocialProposalTargetInput {
  return {
    channel_id: target.channel_id,
    text: target.text,
    image_url: target.image_url?.trim() || null,
    schedule_mode: target.schedule_mode,
    due_at:
      target.schedule_mode === "scheduled"
        ? rfc3339Value(target.due_at ?? "")
        : null,
    utm: {
      source: target.utm.source?.trim() || null,
      medium: target.utm.medium?.trim() || null,
      campaign: target.utm.campaign?.trim() || null,
      content: target.utm.content?.trim() || null,
    },
  };
}

export function targetReadyForProvider(
  target: SocialProposalTargetInput,
  platform: string | undefined,
): boolean {
  return platform?.trim().toLowerCase() !== "instagram" || Boolean(target.image_url?.trim());
}

export function nextSocialProposalId(
  entries: readonly SocialPostProposalWithRevision[],
  currentId: string | null,
  key: "ArrowDown" | "ArrowUp" | "j" | "k" | "Home" | "End",
): string | null {
  if (entries.length === 0) return null;
  if (key === "Home") return entries[0].proposal.proposal_id;
  if (key === "End") return entries[entries.length - 1].proposal.proposal_id;
  const currentIndex = Math.max(
    0,
    entries.findIndex((entry) => entry.proposal.proposal_id === currentId),
  );
  const direction = key === "ArrowDown" || key === "j" ? 1 : -1;
  return entries[(currentIndex + direction + entries.length) % entries.length].proposal.proposal_id;
}

function browserTimeZoneLabel(): string {
  const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  return zone ? `Your time · ${zone}` : "Your local time";
}

export default function SocialPublishing({
  onUnauthorized,
  helpTopicId,
  onOpenHelpTopic,
}: {
  onUnauthorized: () => void;
  helpTopicId?: string;
  onOpenHelpTopic: (topicId: string) => void;
}) {
  const [proposals, setProposals] = useState<SocialPostProposalWithRevision[]>([]);
  const [channels, setChannels] = useState<SocialPublishingChannel[]>([]);
  const [sources, setSources] = useState<SocialPublishedSource[]>([]);
  const [bufferConfigured, setBufferConfigured] = useState(false);
  const [liveEnabled, setLiveEnabled] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [notice, setNotice] = useState<Notice>(null);
  const [sourceId, setSourceId] = useState("");
  const [canonicalUrl, setCanonicalUrl] = useState("");
  const [targets, setTargets] = useState<SocialProposalTargetInput[]>([]);
  const proposalButtonRefs = useRef<Record<string, HTMLButtonElement | null>>({});

  const load = useCallback(async () => {
    setRefreshing(true);
    try {
      const response = await api.socialProposals();
      setProposals(response.proposals);
      setChannels(response.channels);
      setSources(response.published_sources);
      setBufferConfigured(response.buffer_configured);
      setLiveEnabled(response.buffer_live_enabled);
      setSelectedId((current) =>
        current === ""
          ? ""
          : response.proposals.some(
                (entry) => entry.proposal.proposal_id === current,
              )
            ? current
            : (response.proposals[0]?.proposal.proposal_id ?? null),
      );
      setTargets((current) =>
        current.length > 0
          ? current
          : response.channels.map((channel) => targetInput(channel)),
      );
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice({ kind: "error", text: errorMessage(err) });
    } finally {
      setLoaded(true);
      setRefreshing(false);
    }
  }, [onUnauthorized]);

  useEffect(() => {
    void load();
  }, [load]);

  const hasPending = proposals.some((entry) =>
    entry.proposal.targets.some((target) => target.outbox_job?.status === "pending"),
  );
  const hasGenerating = sources.some(
    (source) => source.generation_status === "generating",
  );
  useEffect(() => {
    if (!hasPending && !hasGenerating) return;
    const timer = window.setInterval(() => void load(), 5_000);
    return () => window.clearInterval(timer);
  }, [hasGenerating, hasPending, load]);

  const selected = useMemo(
    () =>
      selectedId
        ? (proposals.find(
            (entry) => entry.proposal.proposal_id === selectedId,
          ) ?? null)
        : null,
    [proposals, selectedId],
  );

  const moveProposalFocus = (
    key: "ArrowDown" | "ArrowUp" | "j" | "k" | "Home" | "End",
  ) => {
    const nextId = nextSocialProposalId(proposals, selected?.proposal.proposal_id ?? null, key);
    if (!nextId) return;
    setSelectedId(nextId);
    window.requestAnimationFrame(() => proposalButtonRefs.current[nextId]?.focus());
  };

  useEffect(() => {
    if (!selected || selected.proposal.status !== "staged") return;
    setCanonicalUrl(selected.proposal.canonical_url);
    setSourceId(selected.proposal.source_id ?? "");
    setTargets(
      selected.proposal.targets.map((target) => ({
        channel_id: target.channel_id,
        text: target.text,
        image_url: target.image_url ?? null,
        utm: target.utm,
        schedule_mode: target.schedule_mode,
        due_at: target.due_at ?? null,
      })),
    );
  }, [selected]);

  const beginNew = () => {
    setSelectedId("");
    setSourceId("");
    setCanonicalUrl("");
    setTargets(channels.map((channel) => targetInput(channel)));
    setNotice(null);
  };

  const chooseSource = (nextId: string) => {
    setSourceId(nextId);
    const source = sources.find((item) => item.source_id === nextId);
    if (!source) return;
    setCanonicalUrl(source.canonical_url);
    setTargets(channels.map((channel) => targetInput(channel, source)));
  };

  const selectedSource = sources.find((source) => source.source_id === sourceId);

  const generate = async () => {
    if (!selectedSource) return;
    setBusy("generate");
    setNotice(null);
    try {
      await api.generateSocialProposal(selectedSource.source_id, {
        expected_revision: selectedSource.revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNotice({
        kind: "success",
        text: "BusinessOS is drafting grounded copy for every channel.",
      });
      await load();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else if (isRevisionConflict(err)) {
        setNotice({ kind: "conflict", text: "Changed elsewhere — reload." });
        await load();
      } else setNotice({ kind: "error", text: errorMessage(err) });
    } finally {
      setBusy(null);
    }
  };

  const patchTarget = (
    channelId: string,
    patch: (target: SocialProposalTargetInput) => SocialProposalTargetInput,
  ) => {
    setTargets((current) =>
      current.map((target) =>
        target.channel_id === channelId ? patch(target) : target,
      ),
    );
  };

  const save = async () => {
    if (!canonicalUrl.trim() || targets.some((target) => !target.text.trim())) return;
    setBusy("save");
    setNotice(null);
    try {
      if (selected?.proposal.status === "staged") {
        await api.updateSocialProposal(selected.proposal.proposal_id, {
          canonical_url: canonicalUrl,
          targets: targets.map(targetRequest),
          expected_revision: selected.revision,
          idempotency_key: crypto.randomUUID(),
          actor_id: null,
        });
        setNotice({ kind: "success", text: "Proposal saved." });
      } else {
        await api.stageSocialProposal({
          source_id: selectedSource?.source_id ?? null,
          source_content_draft_id: selectedSource?.source_content_draft_id ?? null,
          source_content_draft_revision:
            selectedSource?.source_content_draft_revision ?? null,
          canonical_url: canonicalUrl,
          targets: targets.map(targetRequest),
          idempotency_key: crypto.randomUUID(),
          actor_id: null,
        });
        setSelectedId(null);
        setNotice({ kind: "success", text: "Proposal staged for approval." });
      }
      await load();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else if (isRevisionConflict(err)) {
        setNotice({ kind: "conflict", text: "Changed elsewhere — reload." });
        await load();
      } else setNotice({ kind: "error", text: errorMessage(err) });
    } finally {
      setBusy(null);
    }
  };

  const decide = async (action: "approve" | "reject") => {
    if (!selected || selected.proposal.status !== "staged") return;
    setBusy(action);
    setNotice(null);
    try {
      await api.actionSocialProposal(selected.proposal.proposal_id, {
        action,
        expected_revision: selected.revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      setNotice({
        kind: "success",
        text:
          action === "approve"
            ? liveEnabled
              ? "Approved. Each Buffer channel is queued independently."
              : "Approved. Channel delivery is running in dry-run mode."
            : "Proposal rejected.",
      });
      await load();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else if (isRevisionConflict(err)) {
        setNotice({ kind: "conflict", text: "Changed elsewhere — reload." });
        await load();
      } else setNotice({ kind: "error", text: errorMessage(err) });
    } finally {
      setBusy(null);
    }
  };

  const editing = !selected || selected.proposal.status === "staged";
  const hasUnsavedChanges = Boolean(
    selected?.proposal.status === "staged" &&
      (canonicalUrl.trim() !== selected.proposal.canonical_url ||
        JSON.stringify(targets.map(targetRequest)) !==
          JSON.stringify(selected.proposal.targets.map(targetRequest))),
  );
  const targetsReadyForProviders = targets.every((target) =>
    targetReadyForProvider(
      target,
      channels.find((channel) => channel.channel_id === target.channel_id)?.platform,
    ),
  );
  const canSave =
    bufferConfigured &&
    canonicalUrl.trim().length > 0 &&
    targets.length === channels.length &&
    targets.every(
      (target) =>
        target.text.trim().length > 0 &&
        (target.schedule_mode === "queue" || Boolean(target.due_at)),
    );

  return (
    <div className="min-w-0 space-y-5">
      <div className="flex flex-col items-start justify-between gap-3 sm:flex-row">
        <div className="min-w-0">
          <div className="flex items-center gap-2">
            <h1 className="text-lg font-semibold text-zinc-100">Social publishing</h1>
            {helpTopicId ? (
              <SectionHelpButton
                topicId={helpTopicId}
                label="Social publishing help"
                onOpenHelp={onOpenHelpTopic}
              />
            ) : null}
          </div>
          <p className="mt-1 max-w-2xl text-sm text-zinc-400">
            Edit each network's exact payload, then approve one revision. Every channel reports
            delivery independently; only known failures can be retried.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <StatusBadge tone={liveEnabled ? "ok" : "warning"}>
            {liveEnabled ? "Buffer writes live" : "Buffer dry run"}
          </StatusBadge>
          <Button variant="primary" onClick={beginNew} disabled={!bufferConfigured}>
            New proposal
          </Button>
          <Button variant="ghost" busy={refreshing} onClick={() => void load()}>
            Refresh
          </Button>
        </div>
      </div>

      {notice ? (
        <div
          role={notice.kind === "success" ? "status" : "alert"}
          className={`rounded-md border px-3 py-2 text-sm ${
            notice.kind === "success"
              ? "border-emerald-500/30 bg-emerald-500/10 text-emerald-300"
              : notice.kind === "conflict"
                ? "border-amber-500/30 bg-amber-500/10 text-amber-300"
                : "border-red-500/30 bg-red-500/10 text-red-300"
          }`}
        >
          {notice.text}
        </div>
      ) : null}

      {!bufferConfigured ? (
        <Surface
          accent="amber"
          title="Buffer channels not configured"
          subtitle="Social destinations are set up by your administrator in deployment settings."
        >
          <p className="text-sm text-zinc-400">
            No proposals can be staged until BusinessOS knows the exact allowed channel IDs.
          </p>
        </Surface>
      ) : null}

      <div className="grid min-w-0 gap-4 lg:grid-cols-[20rem_minmax(0,1fr)]">
        <Surface
          accent="violet"
          title="Proposals"
          titleAs="h2"
          subtitle="Newest first · staged items need a human decision"
          className="min-w-0"
          bodyClassName="max-h-80 overflow-y-auto p-0 lg:max-h-none"
        >
          {!loaded ? (
            <SkeletonList rows={4} />
          ) : notice?.kind === "error" && proposals.length === 0 ? (
            <div className="p-4">
              <EmptyState
                title="Proposals could not be loaded"
                action={
                  <Button variant="secondary" busy={refreshing} onClick={() => void load()}>
                    {refreshing ? "Retrying…" : "Retry"}
                  </Button>
                }
              >
                Check the connection and try again. No proposal was changed.
              </EmptyState>
            </div>
          ) : proposals.length === 0 ? (
            <div className="p-4">
              <EmptyState title="No social proposals">
                Start after a blog post has a canonical published URL.
              </EmptyState>
            </div>
          ) : (
            <div className="divide-y divide-zinc-800">
              {proposals.map((entry) => (
                <button
                  key={entry.proposal.proposal_id}
                  type="button"
                  ref={(node) => {
                    proposalButtonRefs.current[entry.proposal.proposal_id] = node;
                  }}
                  aria-current={selected?.proposal.proposal_id === entry.proposal.proposal_id ? "true" : undefined}
                  onClick={() => setSelectedId(entry.proposal.proposal_id)}
                  onKeyDown={(event) => {
                    if (!["ArrowDown", "ArrowUp", "j", "k", "Home", "End"].includes(event.key)) return;
                    event.preventDefault();
                    moveProposalFocus(event.key as "ArrowDown" | "ArrowUp" | "j" | "k" | "Home" | "End");
                  }}
                  className={`w-full border-l-2 px-4 py-3 text-left transition hover:bg-zinc-800/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-sky-500/70 ${
                    selected?.proposal.proposal_id === entry.proposal.proposal_id
                      ? "border-l-sky-500 bg-zinc-800/70"
                      : "border-l-transparent"
                  }`}
                >
                  <div className="flex items-center justify-between gap-2">
                    <span className="min-w-0 flex-1 truncate text-sm font-medium text-zinc-200">
                      {new URL(entry.proposal.canonical_url).pathname}
                    </span>
                    <StatusBadge tone={statusTone(entry.proposal.status)}>
                      {statusLabel(entry.proposal.status)}
                    </StatusBadge>
                  </div>
                  <div className="mt-1 text-xs text-zinc-400">
                    {entry.proposal.targets.length} channel
                    {entry.proposal.targets.length === 1 ? "" : "s"} · revision {entry.revision}
                  </div>
                </button>
              ))}
            </div>
          )}
        </Surface>

        <div className="min-w-0 space-y-4 pb-24">
          <Surface
            accent="sky"
            title={selected ? "Review proposal" : "New proposal"}
            titleAs="h2"
            subtitle={
              editing
                ? "The text shown here becomes the immutable approval snapshot."
                : "Approved payload snapshot and per-channel delivery result."
            }
            actions={
              selected ? (
                <StatusBadge tone={statusTone(selected.proposal.status)}>
                  {statusLabel(selected.proposal.status)}
                </StatusBadge>
              ) : undefined
            }
          >
            {editing ? (
              <div className="space-y-4">
                {!selected && sources.length > 0 ? (
                  <label className="block text-xs font-medium text-zinc-400">
                    Published source
                    <select
                      value={sourceId}
                      onChange={(event) => chooseSource(event.target.value)}
                      className="mt-1 w-full rounded-md border border-zinc-700 bg-zinc-950 px-3 py-2 text-sm text-zinc-200 outline-none focus-visible:border-sky-600 focus-visible:ring-2 focus-visible:ring-sky-500/30"
                    >
                      <option value="">Use another canonical URL</option>
                      {sources.map((source) => (
                        <option key={source.source_id} value={source.source_id}>
                          {source.title}
                        </option>
                      ))}
                    </select>
                  </label>
                ) : null}
                {!selected && selectedSource ? (
                  <div className="flex flex-wrap items-center gap-2 rounded-md border border-violet-500/20 bg-violet-500/5 p-3">
                    <Button
                      variant="primary"
                      busy={busy === "generate"}
                      disabled={
                        Boolean(busy) ||
                        selectedSource.generation_status === "generating" ||
                        selectedSource.generation_status === "proposal_staged"
                      }
                      onClick={() => void generate()}
                    >
                      {selectedSource.generation_status === "generating"
                        ? "Drafting…"
                        : selectedSource.generation_status === "proposal_staged"
                          ? "Proposal staged"
                          : "Generate with AI"}
                    </Button>
                    <span className="text-xs text-zinc-400">
                      BusinessOS sends published content and channel names to the routed typed
                      transform. Approval remains here.
                    </span>
                    {selectedSource.generation_error ? (
                      <span className="w-full text-xs text-red-300">
                        Drafting failed: {selectedSource.generation_error}
                      </span>
                    ) : null}
                  </div>
                ) : null}
                <label className="block text-xs font-medium text-zinc-400">
                  Canonical published URL
                  <input
                    type="url"
                    value={canonicalUrl}
                    onChange={(event) => setCanonicalUrl(event.target.value)}
                    disabled={Boolean(sourceId)}
                    placeholder="https://example.com/blog/post"
                    className="mt-1 w-full rounded-md border border-zinc-700 bg-zinc-950 px-3 py-2 text-sm text-zinc-200 outline-none focus-visible:border-sky-600 focus-visible:ring-2 focus-visible:ring-sky-500/30 disabled:opacity-60"
                  />
                </label>
                {selectedSource ? (
                  <div className="rounded-md border border-zinc-800 bg-zinc-950/70 p-3 text-xs">
                    <div className="font-medium text-zinc-200">Source: {selectedSource.title}</div>
                    <div className="mt-1 flex flex-wrap gap-x-3 gap-y-1 text-zinc-400">
                      <span>{selectedSource.source_kind.replaceAll("_", " ")}</span>
                      <span>Source revision {selectedSource.revision}</span>
                      {selectedSource.source_content_draft_revision ? (
                        <span>Article revision {selectedSource.source_content_draft_revision}</span>
                      ) : null}
                    </div>
                  </div>
                ) : null}
              </div>
            ) : selected ? (
              <a
                href={selected.proposal.canonical_url}
                target="_blank"
                rel="noreferrer"
                className="break-all text-sm text-sky-300 hover:underline"
              >
                {selected.proposal.canonical_url}
              </a>
            ) : null}
          </Surface>

          {(editing ? targets : selected?.proposal.targets ?? []).map((target) => {
            const channel = channels.find((item) => item.channel_id === target.channel_id);
            const stored = selected?.proposal.targets.find(
              (item) => item.channel_id === target.channel_id,
            );
            const platform = (stored?.platform ?? channel?.platform)?.toLowerCase();
            const imageRequired = platform === "instagram";
            const googleBusiness = platform === "googlebusiness";
            return (
              <Surface
                key={target.channel_id}
                accent="zinc"
                title={stored?.channel_name ?? channel?.name ?? target.channel_id}
                titleAs="h2"
                subtitle={stored?.platform ?? channel?.platform ?? "Buffer channel"}
                actions={
                  <span className="text-xs text-zinc-400">
                    {target.text.length.toLocaleString()} chars
                  </span>
                }
              >
                {editing ? (
                  <div className="space-y-3">
                    <label className="block text-xs font-medium text-zinc-400">
                      Post text
                      <textarea
                        rows={5}
                        value={target.text}
                        onChange={(event) =>
                          patchTarget(target.channel_id, (current) => ({
                            ...current,
                            text: event.target.value,
                          }))
                        }
                        className="mt-1 w-full rounded-md border border-zinc-700 bg-zinc-950 px-3 py-2 text-sm leading-relaxed text-zinc-200 outline-none focus-visible:border-sky-600 focus-visible:ring-2 focus-visible:ring-sky-500/30"
                      />
                      {googleBusiness ? (
                        <span className="mt-1 block font-normal text-zinc-400">
                          Keep the tracked URL in this copy. Google Business also
                          uses it on the Learn more button.
                        </span>
                      ) : null}
                    </label>
                    <label className="block text-xs font-medium text-zinc-400">
                      Public image URL{" "}
                      <span className="font-normal text-zinc-400">
                        {imageRequired ? "required for Instagram" : "optional"}
                      </span>
                      <input
                        type="url"
                        value={target.image_url ?? ""}
                        onChange={(event) =>
                          patchTarget(target.channel_id, (current) => ({
                            ...current,
                            image_url: event.target.value || null,
                          }))
                        }
                        className="mt-1 w-full rounded-md border border-zinc-700 bg-zinc-950 px-3 py-2 text-sm text-zinc-200 outline-none focus-visible:border-sky-600 focus-visible:ring-2 focus-visible:ring-sky-500/30"
                      />
                      {imageRequired && !target.image_url?.trim() ? (
                        <span className="mt-1 block font-normal text-amber-300">
                          Add a public HTTPS image before this proposal can be approved.
                        </span>
                      ) : null}
                    </label>
                    <div className="grid gap-2 sm:grid-cols-4">
                      {(["source", "medium", "campaign", "content"] as const).map((key) => (
                        <label key={key} className="text-xs font-medium capitalize text-zinc-400">
                          UTM {key}
                          <input
                            value={target.utm[key] ?? ""}
                            onChange={(event) =>
                              patchTarget(target.channel_id, (current) => ({
                                ...current,
                                utm: { ...current.utm, [key]: event.target.value || null },
                              }))
                            }
                            className="mt-1 w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-200 outline-none focus-visible:border-sky-600 focus-visible:ring-2 focus-visible:ring-sky-500/30"
                          />
                        </label>
                      ))}
                    </div>
                    <div className="grid gap-2 sm:grid-cols-2">
                      <label className="text-xs font-medium text-zinc-400">
                        Scheduling
                        <select
                          value={target.schedule_mode}
                          onChange={(event) =>
                            patchTarget(target.channel_id, (current) => ({
                              ...current,
                              schedule_mode: event.target.value as "queue" | "scheduled",
                              due_at: event.target.value === "queue" ? null : current.due_at,
                            }))
                          }
                          className="mt-1 w-full rounded-md border border-zinc-700 bg-zinc-950 px-3 py-2 text-sm text-zinc-200 outline-none focus-visible:border-sky-600 focus-visible:ring-2 focus-visible:ring-sky-500/30"
                        >
                          <option value="queue">Next queue slot</option>
                          <option value="scheduled">Specific time</option>
                        </select>
                      </label>
                      {target.schedule_mode === "scheduled" ? (
                        <label className="text-xs font-medium text-zinc-400">
                          Publish time
                          <input
                            type="datetime-local"
                            value={localDateTimeValue(target.due_at)}
                            onChange={(event) =>
                              patchTarget(target.channel_id, (current) => ({
                                ...current,
                                due_at: event.target.value,
                              }))
                            }
                            className="mt-1 w-full rounded-md border border-zinc-700 bg-zinc-950 px-3 py-2 text-sm text-zinc-200 outline-none focus-visible:border-sky-600 focus-visible:ring-2 focus-visible:ring-sky-500/30"
                          />
                          <span className="mt-1 block font-normal text-zinc-400">
                            {browserTimeZoneLabel()}
                          </span>
                        </label>
                      ) : null}
                    </div>
                  </div>
                ) : stored ? (
                  <div className="space-y-3">
                    <div className="break-words whitespace-pre-wrap rounded-md bg-zinc-950/70 p-3 text-sm leading-relaxed text-zinc-200 [overflow-wrap:anywhere]">
                      {stored.text}
                    </div>
                    <dl className="grid gap-2 text-xs sm:grid-cols-2">
                      <div>
                        <dt className="text-zinc-400">Tracked URL</dt>
                        <dd className="break-all text-zinc-300">{stored.tracked_url}</dd>
                      </div>
                      <div>
                        <dt className="text-zinc-400">Schedule</dt>
                        <dd className="text-zinc-300">
                          {stored.schedule_mode === "queue"
                            ? "Next Buffer queue slot"
                            : `${new Date(stored.due_at ?? "").toLocaleString()} · ${browserTimeZoneLabel()}`}
                        </dd>
                      </div>
                    </dl>
                    <OutboxStateLine
                      job={stored.outbox_job}
                      show={Boolean(stored.outbox_job)}
                      dryRunText="Validated without creating a Buffer post."
                      deliveredText={(job) =>
                        job.provider_object_id
                          ? `Buffer post ${job.provider_object_id} created.`
                          : "Buffer post created."
                      }
                      onUnauthorized={onUnauthorized}
                      onRetried={load}
                    />
                  </div>
                ) : null}
              </Surface>
            );
          })}

          {editing ? (
            <div className="sticky bottom-0 flex flex-wrap items-center justify-end gap-2 rounded-lg border border-zinc-800 bg-zinc-950/95 p-3 shadow-xl backdrop-blur">
              {selected && hasUnsavedChanges ? (
                <span className="mr-auto text-xs text-amber-300">
                  Save changes before approval.
                </span>
              ) : selected ? (
                <span className="mr-auto text-xs text-zinc-400">
                  {liveEnabled
                    ? `Approval queues ${targets.length} live Buffer post${targets.length === 1 ? "" : "s"}.`
                    : "Approval validates every channel without creating a Buffer post."}
                </span>
              ) : null}
              {selected ? (
                <Button
                  variant="danger"
                  busy={busy === "reject"}
                  disabled={Boolean(busy)}
                  onClick={() => void decide("reject")}
                >
                  Reject
                </Button>
              ) : null}
              <Button
                variant="secondary"
                busy={busy === "save"}
                disabled={Boolean(busy) || !canSave}
                onClick={() => void save()}
              >
                {selected ? "Save changes" : "Stage proposal"}
              </Button>
              {selected ? (
                <Button
                  variant="success"
                  busy={busy === "approve"}
                  disabled={Boolean(busy) || hasUnsavedChanges || !targetsReadyForProviders}
                  title={
                    hasUnsavedChanges
                      ? "Save changes before approval"
                      : !targetsReadyForProviders
                        ? "Instagram needs a public image before approval"
                        : undefined
                  }
                  onClick={() => void decide("approve")}
                >
                  {liveEnabled ? "Approve & queue in Buffer" : "Approve dry run"}
                </Button>
              ) : null}
            </div>
          ) : null}
        </div>
      </div>
    </div>
  );
}
