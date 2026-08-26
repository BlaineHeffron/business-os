import { useCallback, useEffect, useRef, useState } from "react";
import type { EmailTriageConditionOperator } from "../types/generated/EmailTriageConditionOperator";
import type { EmailTriageConditionValue } from "../types/generated/EmailTriageConditionValue";
import type { EmailTriageRule } from "../types/generated/EmailTriageRule";
import type { RuleWithRevision } from "../types/generated/RuleWithRevision";
import {
  api,
  errorMessage,
  isRevisionConflict,
  isUnauthorized,
} from "../lib/api";
import { useAppCommand } from "../lib/commands";
import CategoryBadge from "../components/CategoryBadge";
import DryRunPanel from "../components/DryRunPanel";
import RuleEditor from "../components/RuleEditor";
import SectionHelpButton from "../components/SectionHelpButton";
import {
  Button,
  ConfirmDialog,
  EmptyState,
  cellCls,
  rowDivideCls,
  rowHoverCls,
  tableCls,
  tableWrapCls,
  theadCls,
} from "../components/ui";

type Notice = { text: string; kind: "error" | "conflict" | "info" } | null;

const FIELD_LABELS: Record<string, string> = {
  sender_in_crm_contacts: "sender in CRM contacts",
  sender_domain_in_crm_companies: "sender domain in CRM companies",
};

function conditionValueLabel(value: EmailTriageConditionValue): string {
  if (typeof value === "string") return "";
  if ("text" in value) return value.text;
  if ("header" in value) return `${value.header.name}: ${value.header.value}`;
  if ("bool" in value) return String(value.bool);
  if ("number" in value) return String(value.number);
  if ("money_cents" in value) return String(value.money_cents);
  if ("date" in value) return value.date;
  if ("string_list" in value) return value.string_list.join(", ");
  return "";
}

function opLabel(op: EmailTriageConditionOperator): string {
  switch (op) {
    case "contains":
      return "contains";
    case "equals":
      return "is";
    case "starts_with":
      return "starts with";
    case "regex":
      return "matches";
    case "exists":
      return "is known";
    case "is_true":
      return "is";
    case "is_false":
      return "is not";
    case "in":
      return "is one of";
    case "greater_than":
      return "is greater than";
    case "less_than":
      return "is less than";
    case "at_least":
      return "is at least";
    case "at_most":
      return "is at most";
  }
}

function summarizeRule(
  rule: EmailTriageRule,
  conditionLabels: Record<string, string>,
): string {
  const joiner = rule.match_mode === "all" ? " AND " : " OR ";
  if (rule.conditions_v2.length > 0) {
    return rule.conditions_v2
      .map((condition) => {
        const label = conditionLabels[condition.condition_id] ?? "Condition";
        const value = conditionValueLabel(condition.value);
        return `${label} ${opLabel(condition.op)}${value ? ` "${value}"` : ""}`;
      })
      .join(joiner);
  }
  return rule.conditions
    .map(
      (c) =>
        `${FIELD_LABELS[c.field] ?? c.field}${
          c.header_name ? `[${c.header_name}]` : ""
        } ${c.op}${c.op === "exists" ? "" : ` "${c.value}"`}`,
    )
    .join(joiner);
}

export default function Rules({
  onUnauthorized,
  helpTopicId,
  onOpenHelpTopic,
  seed,
  onSeedConsumed,
  aiTriageEnabled,
}: {
  onUnauthorized: () => void;
  helpTopicId?: string;
  onOpenHelpTopic: (topicId: string) => void;
  seed?: EmailTriageRule | null;
  onSeedConsumed?: () => void;
  aiTriageEnabled: boolean;
}) {
  const [rules, setRules] = useState<RuleWithRevision[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<Notice>(null);
  const [editorOpen, setEditorOpen] = useState(false);
  const [editing, setEditing] = useState<RuleWithRevision | null>(null);
  const [conditionLabels, setConditionLabels] = useState<Record<string, string>>(
    {},
  );
  const [busyRuleId, setBusyRuleId] = useState<string | null>(null);
  const [dryRunRequest, setDryRunRequest] = useState<{
    ruleId: string;
    seq: number;
    source: "draft" | "saved";
    proposedRule: EmailTriageRule | null;
  } | null>(null);
  const [previewSummary, setPreviewSummary] = useState<{
    matched: number;
    total: number;
    loading: boolean;
  } | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<RuleWithRevision | null>(
    null,
  );
  const dryRunSeqRef = useRef(0);

  const clearPreviewState = useCallback(() => {
    setDryRunRequest(null);
    setPreviewSummary(null);
  }, []);

  const load = useCallback(async () => {
    try {
      const res = await api.rules();
      setRules(
        [...res.rules].sort((a, b) => a.rule.priority - b.rule.priority),
      );
      setError(null);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setError(errorMessage(err));
    } finally {
      setLoaded(true);
    }
  }, [onUnauthorized]);

  useEffect(() => {
    let cancelled = false;
    api
      .conditionCatalog()
      .then((catalog) => {
        if (cancelled) return;
        const labels: Record<string, string> = {};
        for (const group of catalog.groups) {
          for (const item of group.items) labels[item.condition_id] = item.label;
        }
        setConditionLabels(labels);
      })
      .catch((err) => {
        if (isUnauthorized(err)) onUnauthorized();
      });
    return () => {
      cancelled = true;
    };
  }, [onUnauthorized]);

  useAppCommand("refresh", () => void load());
  useAppCommand("rules.new", () => {
    setEditing(null);
    clearPreviewState();
    setEditorOpen(true);
  });

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (seed) {
      setEditing(null);
      clearPreviewState();
      setEditorOpen(true);
    }
  }, [clearPreviewState, seed]);

  const runToggle = async (
    entry: RuleWithRevision,
    action: "enable" | "disable",
  ) => {
    if (busyRuleId === entry.rule.rule_id) return;
    setBusyRuleId(entry.rule.rule_id);
    setNotice(null);
    // Snapshot before any mutation
    const snapshot = entry;
    // Optimistic update: flip enabled immediately
    setRules((prev) =>
      prev.map((r) =>
        r.rule.rule_id === entry.rule.rule_id
          ? { ...r, rule: { ...r.rule, enabled: action === "enable" } }
          : r,
      ),
    );
    try {
      const res = await api.ruleAction(entry.rule.rule_id, {
        action,
        expected_revision: entry.revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      // Patch revision from server response (no full reload needed)
      const newRevision = res.revision ?? snapshot.revision + 1;
      setRules((prev) =>
        prev.map((r) =>
          r.rule.rule_id === entry.rule.rule_id
            ? { ...r, revision: newRevision }
            : r,
        ),
      );
    } catch (err) {
      if (isUnauthorized(err)) {
        onUnauthorized();
      } else if (isRevisionConflict(err)) {
        setNotice({
          text: "Rule changed elsewhere — reloaded the latest revisions.",
          kind: "conflict",
        });
        await load();
      } else {
        // Revert optimistic change
        setRules((prev) =>
          prev.map((r) =>
            r.rule.rule_id === snapshot.rule.rule_id ? snapshot : r,
          ),
        );
        setNotice({
          text: `Action failed: ${errorMessage(err)}`,
          kind: "error",
        });
      }
    } finally {
      setBusyRuleId(null);
    }
  };

  const runDelete = async (entry: RuleWithRevision) => {
    setBusyRuleId(entry.rule.rule_id);
    setNotice(null);
    try {
      await api.ruleAction(entry.rule.rule_id, {
        action: "delete",
        expected_revision: entry.revision,
        idempotency_key: crypto.randomUUID(),
        actor_id: null,
      });
      await load();
    } catch (err) {
      if (isUnauthorized(err)) {
        onUnauthorized();
      } else if (isRevisionConflict(err)) {
        setNotice({
          text: "Rule changed elsewhere — reloaded the latest revisions.",
          kind: "conflict",
        });
        await load();
      } else {
        setNotice({
          text: `Action failed: ${errorMessage(err)}`,
          kind: "error",
        });
      }
    } finally {
      setBusyRuleId(null);
    }
  };

  const onDraftChange = useCallback(
    (rule: EmailTriageRule | null, dirty: boolean) => {
      void rule;
      if (dirty && dryRunRequest?.source === "draft") {
        clearPreviewState();
      }
    },
    [clearPreviewState, dryRunRequest?.source],
  );

  const closeEditor = () => {
    setEditorOpen(false);
    setEditing(null);
    clearPreviewState();
    onSeedConsumed?.();
  };

  const onPreviewSummaryChange = useCallback(
    (
      summary: { matched: number; total: number; loading: boolean } | null,
    ) => {
      setPreviewSummary(summary);
    },
    [],
  );

  return (
    <div className="flex flex-col gap-4">
      <div className="surface-section-head surface-head-violet flex items-center justify-between">
        <div className="flex items-center gap-2">
          <h2 className="text-lg font-semibold text-zinc-100">Triage rules</h2>
          <SectionHelpButton
            topicId={helpTopicId}
            onOpenHelp={onOpenHelpTopic}
            label="Open help for Rules"
          />
        </div>
        <div className="flex items-center gap-2">
          <Button
            variant="secondary"
            size="md"
            onClick={() => {
              setNotice(null);
              void (async () => {
                try {
                  const res = await api.reclassify();
                  setNotice({
                    text:
                      `Re-ran rules over ${res.examined} stored messages: ` +
                      `${res.reclassified} reclassified, ${res.work_items_emitted} work item${
                        res.work_items_emitted === 1 ? "" : "s"
                      } added to the queue.`,
                    kind: "info",
                  });
                  await load();
                } catch (err) {
                  if (isUnauthorized(err)) onUnauthorized();
                  else
                    setNotice({
                      text: `Re-run failed: ${errorMessage(err)}`,
                      kind: "error",
                    });
                }
              })();
            }}
            title="Apply the current rules to existing mail and update the queue"
          >
            Re-run rules
          </Button>
          {aiTriageEnabled ? (
            <Button
              variant="secondary"
              size="md"
              onClick={() => {
                if (!aiTriageEnabled) return;
                setNotice(null);
                void (async () => {
                  try {
                    const res = await api.aiRetriageReset({
                      scope: "stale",
                      source_key: null,
                      message_id: null,
                      idempotency_key: crypto.randomUUID(),
                      actor_id: null,
                    });
                    setNotice({
                      text:
                        res.reset === 0
                          ? "No stale AI verdicts were found. This does not re-run rules."
                          : `Re-checking ${res.reset} message${
                              res.reset === 1 ? "" : "s"
                            } with AI. Updates will appear shortly.`,
                      kind: "info",
                    });
                  } catch (err) {
                    if (isUnauthorized(err)) onUnauthorized();
                    else
                      setNotice({
                        text: `AI re-triage reset failed: ${errorMessage(err)}`,
                        kind: "error",
                      });
                  }
                })();
              }}
              title="Clear stale AI decisions so AI can review them again; this does not re-run rules"
            >
              Re-check AI sorting
            </Button>
          ) : null}
          <Button
            variant="primary"
            size="md"
            onClick={() => {
              setEditing(null);
              clearPreviewState();
              setEditorOpen(true);
            }}
          >
            + New rule
          </Button>
        </div>
      </div>

      {error ? (
        <div className="rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300">
          Failed to load rules: {error}
        </div>
      ) : null}
      {notice ? (
        <div
          className={
            notice.kind === "error"
              ? "rounded-md border border-red-900/60 bg-red-950/40 px-3 py-2 text-sm text-red-300"
              : "rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-2 text-sm text-amber-300"
          }
        >
          {notice.text}
        </div>
      ) : null}

      {loaded && rules.length === 0 && !error ? (
        <EmptyState
          title="No rules configured."
          action={
            <Button
              variant="primary"
              size="sm"
              onClick={() => {
                setEditing(null);
                clearPreviewState();
                setEditorOpen(true);
              }}
            >
              Create rule
            </Button>
          }
        >
          Mail that does not match a rule goes to AI sorting when it is enabled,
          or to the default category otherwise. Add a rule to route specific
          senders or subjects automatically.
        </EmptyState>
      ) : null}

      {rules.length > 0 ? (
        <div className={`${tableWrapCls} surface-flat surface-body-violet`}>
          <table className={tableCls}>
            <thead className={`${theadCls} surface-head-violet`}>
              <tr>
                <th
                  className={`cursor-help ${cellCls}`}
                  title="Evaluation order — lower number runs first. First matching rule wins; if nothing matches, AI sorting or the default category handles it."
                >
                  Priority
                </th>
                <th className={cellCls}>Rule</th>
                <th
                  className={`cursor-help ${cellCls}`}
                  title="all = every condition must match; any = at least one condition must match"
                >
                  Match
                </th>
                <th
                  className={`cursor-help ${cellCls}`}
                  title="Category assigned to matching messages"
                >
                  Pins to
                </th>
                <th
                  className={`cursor-help ${cellCls}`}
                  title="Field conditions evaluated against each inbound message"
                >
                  Conditions
                </th>
                <th className={cellCls}>Enabled</th>
                <th className={cellCls}></th>
              </tr>
            </thead>
            <tbody className={rowDivideCls}>
              {rules.map((entry) => {
                const r = entry.rule;
                const busy = busyRuleId === r.rule_id;
                return (
                  <tr key={r.rule_id} className={rowHoverCls}>
                    <td className={`${cellCls} font-mono text-zinc-400`}>
                      {r.priority}
                    </td>
                    <td className={`${cellCls} font-mono text-zinc-200`}>
                      {r.rule_id}
                    </td>
                    <td className={`${cellCls} text-zinc-400`}>{r.match_mode}</td>
                    <td className={cellCls}>
                      <CategoryBadge category={r.pinned_category} />
                    </td>
                    <td className={`max-w-md ${cellCls} text-xs text-zinc-400`}>
                      {summarizeRule(r, conditionLabels)}
                    </td>
                    <td className={cellCls}>
                      <button
                        onClick={() =>
                          void runToggle(entry, r.enabled ? "disable" : "enable")
                        }
                        disabled={busy}
                        role="switch"
                        aria-checked={r.enabled}
                        aria-label={r.enabled ? "Disable rule" : "Enable rule"}
                        className={`relative inline-flex h-5 w-9 items-center rounded-full transition disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70 ${
                          r.enabled
                            ? "bg-[var(--success)]"
                            : "bg-[var(--toggle-off)]"
                        }`}
                        title={r.enabled ? "Disable rule" : "Enable rule"}
                      >
                        <span
                          className={`inline-block h-4 w-4 transform rounded-full bg-white transition ${
                            r.enabled ? "translate-x-4.5" : "translate-x-0.5"
                          }`}
                        />
                      </button>
                    </td>
                    <td className={`whitespace-nowrap ${cellCls} text-right`}>
                      <Button
                        variant="secondary"
                        size="sm"
                        disabled={busy}
                        className="mr-2 border-sky-800/70 text-sky-400 hover:bg-sky-950/40"
                        onClick={() => {
                          dryRunSeqRef.current += 1;
                          setDryRunRequest({
                            ruleId: r.rule_id,
                            seq: dryRunSeqRef.current,
                            source: "saved",
                            proposedRule: null,
                          });
                        }}
                      >
                        Dry-run
                      </Button>
                      <Button
                        variant="secondary"
                        size="sm"
                        disabled={busy}
                        className="mr-2"
                        onClick={() => {
                          setEditing(entry);
                          clearPreviewState();
                          setEditorOpen(true);
                        }}
                      >
                        Edit
                      </Button>
                      <Button
                        variant="danger"
                        size="sm"
                        disabled={busy}
                        onClick={() => setConfirmDelete(entry)}
                      >
                        Delete
                      </Button>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      ) : null}

      <ConfirmDialog
        open={confirmDelete !== null}
        title={`Delete rule "${confirmDelete?.rule.rule_id}"?`}
        body="Messages that matched this rule will go to the default category or be sorted by AI. This cannot be undone."
        confirmLabel="Delete rule"
        onConfirm={() => {
          if (!confirmDelete) return;
          const entry = confirmDelete;
          setConfirmDelete(null);
          void runDelete(entry);
        }}
        onCancel={() => setConfirmDelete(null)}
      />

      {editorOpen ? (
        <div className="grid gap-4 xl:grid-cols-[minmax(0,1fr)_minmax(22rem,28rem)]">
          <RuleEditor
            seed={seed}
            editing={editing}
            previewSummary={
              dryRunRequest?.source === "draft" ? previewSummary : null
            }
            onSaved={() => {
              closeEditor();
              void load();
            }}
            onCancel={closeEditor}
            onUnauthorized={onUnauthorized}
            onConflict={() => void load()}
            onDraftChange={onDraftChange}
            onTestDraft={(rule) => {
              setPreviewSummary({
                matched: 0,
                total: 0,
                loading: true,
              });
              dryRunSeqRef.current += 1;
              setDryRunRequest({
                ruleId: rule.rule_id,
                seq: dryRunSeqRef.current,
                source: "draft",
                proposedRule: rule,
              });
            }}
            aiTriageEnabled={aiTriageEnabled}
          />
          {dryRunRequest ? (
            <DryRunPanel
              onUnauthorized={onUnauthorized}
              onPreviewSummaryChange={onPreviewSummaryChange}
              focusedRule={dryRunRequest}
            />
          ) : null}
        </div>
      ) : dryRunRequest ? (
        <DryRunPanel
          onUnauthorized={onUnauthorized}
          onPreviewSummaryChange={onPreviewSummaryChange}
          focusedRule={dryRunRequest}
        />
      ) : null}
    </div>
  );
}
