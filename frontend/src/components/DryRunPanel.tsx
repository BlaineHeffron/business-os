import { useEffect, useRef, useState } from "react";
import type { DryRunResult } from "../types/generated/DryRunResult";
import type { EmailTriageDryRunTrace } from "../types/generated/EmailTriageDryRunTrace";
import type { EmailTriageFactSource } from "../types/generated/EmailTriageFactSource";
import type { EmailTriageTriValue } from "../types/generated/EmailTriageTriValue";
import type { EmailTriageRule } from "../types/generated/EmailTriageRule";
import type { MessageView } from "../types/generated/MessageView";
import { api, errorMessage, isUnauthorized } from "../lib/api";
import CategoryBadge from "./CategoryBadge";

interface SampleMessage {
  messageId: string | null;
  sourceUserId: string | null;
  subject: string;
  from: string;
  to: string;
  body: string;
  labels: string[];
  currentCategory: string | null;
}

function toMessageView(s: SampleMessage): MessageView {
  return {
    message_id: s.messageId,
    source_user_id: s.sourceUserId,
    subject: s.subject.length > 0 ? s.subject : null,
    from: s.from.length > 0 ? s.from : null,
    to: s.to.length > 0 ? s.to : null,
    body: s.body.length > 0 ? s.body : null,
    labels: s.labels,
    headers: [],
  };
}

function loadInboxSamples(messages: {
  message_id: string;
  source_user_id: string | null;
  subject: string | null;
  from_addr: string | null;
  to_addr: string | null;
  body_excerpt: string;
  labels: string[];
  resolved_category: string;
  ingested_at_ms: number;
}[]): SampleMessage[] {
  return [...messages]
    .sort((a, b) => b.ingested_at_ms - a.ingested_at_ms)
    .map((m) => ({
      messageId: m.message_id,
      sourceUserId: m.source_user_id,
      subject: m.subject ?? "",
      from: m.from_addr ?? "",
      to: m.to_addr ?? "",
      body: m.body_excerpt,
      labels: m.labels,
      currentCategory: m.resolved_category,
    }));
}

export interface DryRunPanelProps {
  onUnauthorized: () => void;
  onPreviewSummaryChange?: (
    summary: { matched: number; total: number; loading: boolean } | null,
  ) => void;
  focusedRule: {
    ruleId: string;
    seq: number;
    source: "draft" | "saved";
    proposedRule: EmailTriageRule | null;
  };
}

const INBOX_SAMPLE_LIMIT = 100;

function triTone(value: EmailTriageTriValue): string {
  if (value === "true") return "border-emerald-800 bg-emerald-950/30 text-emerald-300";
  if (value === "false") return "border-zinc-700 bg-zinc-900 text-zinc-400";
  return "border-amber-800 bg-amber-950/30 text-amber-300";
}

function triLabel(value: EmailTriageTriValue): string {
  if (value === "true") return "Matched";
  if (value === "false") return "Did not match";
  return "Couldn't check";
}

function factSourceLabel(source: EmailTriageFactSource): string {
  switch (source) {
    case "message":
      return "Message";
    case "source":
      return "Mailbox";
    case "accounting_snapshot":
      return "Accounting snapshot";
    case "workflow":
      return "Workflow";
    case "crm_cache":
      return "Saved lookup";
    case "crm_live":
      return "CRM lookup";
    case "not_checked":
      return "Not checked";
  }
}

function focusedRuleLabel(
  focusedRule: DryRunPanelProps["focusedRule"],
): string {
  if (!focusedRule) return "Preview";
  return focusedRule.source === "draft" ? "this draft" : focusedRule.ruleId;
}

export default function DryRunPanel({
  onUnauthorized,
  onPreviewSummaryChange,
  focusedRule,
}: DryRunPanelProps) {
  const [samples, setSamples] = useState<SampleMessage[]>([]);
  const [results, setResults] = useState<DryRunResult[] | null>(null);
  const [traces, setTraces] = useState<EmailTriageDryRunTrace[] | null>(null);
  const [running, setRunning] = useState(false);
  const [loadingInbox, setLoadingInbox] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const focusedRuleRef = useRef(focusedRule);
  focusedRuleRef.current = focusedRule;

  const runPreview = async (sampleSet: SampleMessage[]) => {
    setRunning(true);
    setError(null);
    try {
      const res = await api.dryRun({
        proposed_rules: focusedRuleRef.current.proposedRule
          ? [focusedRuleRef.current.proposedRule]
          : [],
        fallback_category: null,
        samples: sampleSet.map(toMessageView),
      });
      setResults(res.results);
      setTraces(res.traces);
      return res.results;
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
      return null;
    } finally {
      setRunning(false);
    }
  };

  useEffect(() => {
    containerRef.current?.scrollIntoView({ behavior: "smooth", block: "nearest" });

    const loadAndRun = async () => {
      onPreviewSummaryChange?.({
        matched: 0,
        total: samples.length,
        loading: true,
      });
      setLoadingInbox(true);
      setError(null);
      setResults(null);
      setTraces(null);
      try {
        const inbox = await api.inbox();
        const loadedSamples = loadInboxSamples(inbox.messages).slice(
          0,
          INBOX_SAMPLE_LIMIT,
        );
        if (loadedSamples.length === 0) {
          setError("Inbox is empty — nothing to dry run.");
          return;
        }
        setSamples(loadedSamples);
        await runPreview(loadedSamples);
      } catch (err) {
        if (isUnauthorized(err)) onUnauthorized();
        else setError(errorMessage(err));
      } finally {
        setLoadingInbox(false);
      }
    };

    void loadAndRun();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [focusedRule?.seq]);

  const visibleSamples = samples
    .map((sample, index) => ({
      sample,
      index,
      result: results?.[index],
      trace: traces?.[index],
    }))
    .filter(
      ({ result }) =>
        result?.matched_rule_id === focusedRule.ruleId,
    );
  const focusedMatchCount =
    results == null ? null : visibleSamples.length;

  useEffect(() => {
    if (!onPreviewSummaryChange) return;
    if (!focusedRule) {
      onPreviewSummaryChange(null);
      return;
    }
    if (loadingInbox || running) {
      onPreviewSummaryChange({
        matched: focusedMatchCount ?? 0,
        total: samples.length,
        loading: true,
      });
      return;
    }
    if (focusedMatchCount != null) {
      onPreviewSummaryChange({
        matched: focusedMatchCount,
        total: samples.length,
        loading: false,
      });
    }
  }, [
    focusedMatchCount,
    focusedRule,
    loadingInbox,
    onPreviewSummaryChange,
    running,
    samples.length,
  ]);

  return (
    <div ref={containerRef} className="rounded-lg border border-zinc-800 bg-zinc-900/60 p-4">
      <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <h3 className="text-sm font-semibold text-zinc-200">Dry run</h3>
          <span className="rounded bg-sky-900/60 px-1.5 py-0.5 text-xs text-sky-300">
            {focusedRuleLabel(focusedRule)}
          </span>
        </div>
      </div>

      <p className="mb-3 text-xs text-zinc-500">
        {focusedMatchCount != null
          ? `Showing ${focusedMatchCount} inbox ${
              focusedMatchCount === 1 ? "message" : "messages"
            } matched by the selected ${
              focusedRule.source === "draft" ? "draft" : "saved rule"
            }.`
          : `Checking recent inbox messages against the selected ${
              focusedRule.source === "draft" ? "draft" : "saved rule"
            } without saving or changing categories.`}
      </p>

      {(loadingInbox || running) && results == null ? (
        <div className="rounded-md border border-zinc-800 bg-zinc-950/60 px-3 py-6 text-center text-sm text-zinc-500">
          Checking recent inbox messages against this rule…
        </div>
      ) : null}

      <div className="flex flex-col gap-3">
        {visibleSamples.map(({ sample: s, index: i, result, trace }) => {
          const changed =
            result != null &&
            s.currentCategory != null &&
            result.resolved_category !== s.currentCategory;
          const matchesFocused =
            result != null && result.matched_rule_id === focusedRule.ruleId;
          return (
            <div
              key={i}
              className={`rounded-md border p-3 ${
                matchesFocused
                  ? "border-sky-600/60 bg-sky-950/20"
                  : changed
                    ? "border-amber-600/70 bg-amber-950/20"
                    : "border-zinc-800 bg-zinc-950/60"
              }`}
            >
              <div className="mb-2 flex items-center justify-between gap-2">
                <span className="truncate text-xs text-zinc-500">
                  Message {i + 1}
                </span>
              </div>
              <div className="grid grid-cols-1 gap-2 md:grid-cols-2">
                <div className="rounded border border-zinc-800 bg-zinc-950/60 px-3 py-2">
                  <div className="text-[11px] uppercase tracking-wide text-zinc-500">
                    Subject
                  </div>
                  <div className="mt-1 text-sm text-zinc-200">
                    {s.subject.length > 0 ? s.subject : "No subject"}
                  </div>
                </div>
                <div className="rounded border border-zinc-800 bg-zinc-950/60 px-3 py-2">
                  <div className="text-[11px] uppercase tracking-wide text-zinc-500">
                    From
                  </div>
                  <div className="mt-1 text-sm text-zinc-200">
                    {s.from.length > 0 ? s.from : "Unknown sender"}
                  </div>
                </div>
              </div>
              {s.to.length > 0 ? (
                <div className="mt-2 rounded border border-zinc-800 bg-zinc-950/60 px-3 py-2">
                  <div className="text-[11px] uppercase tracking-wide text-zinc-500">
                    To
                  </div>
                  <div className="mt-1 text-sm text-zinc-200">{s.to}</div>
                </div>
              ) : null}
              <div className="mt-2 rounded border border-zinc-800 bg-zinc-950/60 px-3 py-2">
                <div className="text-[11px] uppercase tracking-wide text-zinc-500">
                  Message
                </div>
                <div className="mt-1 whitespace-pre-wrap text-sm text-zinc-200">
                  {s.body.length > 0 ? s.body : "No body excerpt available"}
                </div>
              </div>
              {s.labels.length > 0 ? (
                <div className="mt-1 text-xs text-zinc-500">
                  Labels: {s.labels.join(", ")}
                </div>
              ) : null}

              {result ? (
                <div className="mt-2 flex flex-wrap items-center gap-2 border-t border-zinc-800 pt-2 text-xs">
                  <span className="text-zinc-500">Suggested category</span>
                  <CategoryBadge category={result.resolved_category} />
                  <span className="text-zinc-500">
                    {result.matched_rule_id
                      ? focusedRule.ruleId === result.matched_rule_id
                        ? `Matched ${focusedRuleLabel(focusedRule)}`
                        : `Matched rule ${result.matched_rule_id}`
                      : "No rule matched; using the fallback category"}
                  </span>
                  {s.currentCategory != null ? (
                    <span className="text-zinc-500">
                      {changed ? (
                        <>
                          Current category is{" "}
                          <CategoryBadge category={s.currentCategory} />
                          . This would move it to{" "}
                          <CategoryBadge category={result.resolved_category} />
                          .
                        </>
                      ) : (
                        <>
                          Current category is{" "}
                          <CategoryBadge category={s.currentCategory} />
                          . No change.
                        </>
                      )}
                    </span>
                  ) : null}
                  {matchesFocused ? (
                    <span className="font-semibold text-sky-400">
                      {focusedRule.source === "draft"
                        ? "This draft matched"
                        : "Selected rule matched"}
                    </span>
                  ) : null}
                </div>
              ) : null}
              {trace ? (
                <div className="mt-3 rounded border border-zinc-800 bg-zinc-950/60 px-3 py-2">
                  {trace.needs_fact_refresh ? (
                    <div className="mb-2 rounded border border-amber-900/60 bg-amber-950/30 px-2 py-1 text-xs text-amber-300">
                      Some facts couldn't be checked yet.
                    </div>
                  ) : null}
                  <div className="space-y-2">
                    {trace.rule_traces.map((ruleTrace) => (
                      <div
                        key={ruleTrace.rule_id}
                        className={`rounded border px-2 py-2 ${triTone(ruleTrace.result)}`}
                      >
                        <div className="flex flex-wrap items-center justify-between gap-2 text-xs">
                          <span className="font-medium">
                            {focusedRule.source === "draft" &&
                            ruleTrace.rule_id === focusedRule.ruleId ? (
                              ruleTrace.matched ? (
                                "Matched this draft"
                              ) : (
                                "Checked this draft"
                              )
                            ) : (
                              <>
                                {ruleTrace.matched
                                  ? "Matched rule"
                                  : "Checked rule"}{" "}
                                <span>{ruleTrace.rule_id}</span>
                              </>
                            )}
                          </span>
                          <span>{triLabel(ruleTrace.result)}</span>
                        </div>
                        <div className="mt-2 flex flex-col gap-1">
                          {ruleTrace.condition_traces.map((condition, idx) => (
                            <div
                              key={`${ruleTrace.rule_id}-${idx}`}
                              className={`flex flex-wrap items-center justify-between gap-2 rounded border px-2 py-1 text-xs ${triTone(condition.result)}`}
                            >
                              <span>{condition.label}</span>
                              <span>{condition.detail}</span>
                            </div>
                          ))}
                        </div>
                      </div>
                    ))}
                  </div>
                  {trace.fact_traces.length > 0 ? (
                    <div className="mt-3 border-t border-zinc-800 pt-2">
                      <div className="mb-1 text-[11px] font-semibold uppercase tracking-wide text-zinc-500">
                        Fact checks
                      </div>
                      <div className="flex flex-col gap-1">
                        {trace.fact_traces.map((fact, idx) => (
                          <div
                            key={`${fact.label}-${idx}`}
                            className="flex flex-wrap items-center gap-2 text-xs text-zinc-400"
                          >
                            <span className="text-zinc-200">{fact.label}</span>
                            <span className={`rounded border px-1.5 py-0.5 ${triTone(fact.value)}`}>
                              {triLabel(fact.value)}
                            </span>
                            <span>{factSourceLabel(fact.source)}</span>
                            <span>{fact.detail}</span>
                          </div>
                        ))}
                      </div>
                    </div>
                  ) : null}
                </div>
              ) : null}
            </div>
          );
        })}
      </div>

      {results != null && visibleSamples.length === 0 ? (
        <div className="rounded-md border border-zinc-800 bg-zinc-950/60 px-3 py-6 text-center text-sm text-zinc-500">
          No recent inbox messages matched this rule.
        </div>
      ) : null}

      {error ? (
        <div className="mt-3 rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-xs text-red-300">
          {error}
        </div>
      ) : null}

    </div>
  );
}
