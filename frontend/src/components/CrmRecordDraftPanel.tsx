import type { CrmRecordDraftWithRevision } from "../types/generated/CrmRecordDraftWithRevision";
import type { CrmEnrichmentTrace } from "../types/generated/CrmEnrichmentTrace";
import type { CrmResearchFieldAnnotation } from "../types/generated/CrmResearchFieldAnnotation";
import type { EnrichmentRun } from "../types/generated/EnrichmentRun";
import { useCallback, useEffect, useMemo, useState } from "react";
import { api, errorMessage, isUnauthorized } from "../lib/api";
import { isTerminalEnrichmentStatus } from "../lib/enrichment";
import DraftFieldInput from "./DraftFieldInput";
import { Button } from "./ui";
import {
  useDraftPanel,
  useDraftEdit,
  DraftPanelShell,
  DraftEmptyCta,
  DraftStatusHeader,
  DraftActionFooter,
  OutboxStateLine,
} from "./draft";

type RecordEdit = {
  create_company: boolean;
  company_name: string;
  company_website: string;
  company_phone: string;
  company_address: string;
  company_description: string;
  create_contact: boolean;
  contact_first_name: string;
  contact_last_name: string;
  contact_email: string;
  contact_phone: string;
  contact_title: string;
};

const orEmpty = (v: string | null | undefined) => v ?? "";
const orNull = (v: string) => (v.trim() === "" ? null : v.trim());
const confidenceLabel = (confidence: string) =>
  confidence.slice(0, 1).toUpperCase() + confidence.slice(1);

/** Collapsible "why these values" view: what the read-only crawl fetched, what
 * the deterministic pass + LLM gap-filler extracted, and the exact text fed to
 * the model. Present only while staged (cleared on approval). */
function EnrichmentTraceDetails({ trace }: { trace: CrmEnrichmentTrace }) {
  return (
    <details className="rounded-md border border-zinc-800 bg-zinc-900/40 text-xs">
      <summary className="cursor-pointer select-none px-2 py-1 text-zinc-400 hover:text-zinc-200">
        Web enrichment · {trace.domain}
        {trace.items.length > 0
          ? ` · ${trace.items.length} field${trace.items.length === 1 ? "" : "s"}`
          : " · nothing found"}
      </summary>
      <div className="flex flex-col gap-2 border-t border-zinc-800 px-2 py-2">
        <div>
          <span className="text-zinc-500">Pages fetched ({trace.pages.length}):</span>
          <ul className="mt-0.5 flex flex-col gap-0.5">
            {trace.pages.map((url) => (
              <li key={url} className="truncate font-mono text-zinc-400" title={url}>
                {url}
              </li>
            ))}
          </ul>
        </div>
        {trace.items.length > 0 ? (
          <div className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5">
            {trace.items.map((it, i) => (
              <div key={`${it.field}-${i}`} className="contents">
                <span className="text-zinc-500">
                  {it.field}
                  <span className="ml-1 text-[10px] uppercase text-zinc-600">{it.via}</span>
                </span>
                <span className="text-zinc-200">
                  {it.previous_value ? (
                    <>
                      <span className="text-zinc-500 line-through">{it.previous_value}</span>
                      <span className="px-1 text-zinc-600">→</span>
                    </>
                  ) : null}
                  {it.value}
                  <span
                    className="ml-1 italic text-zinc-500"
                    title="Source quote / page this value came from"
                  >
                    — {it.source}
                  </span>
                </span>
              </div>
            ))}
          </div>
        ) : null}
        {trace.search_ran || trace.failures.length > 0 ? (
          <div>
            <span className="text-zinc-500">
              Search
              {trace.search_reason ? ` · ${trace.search_reason}` : ""}
            </span>
            {trace.search_queries.length > 0 ? (
              <ul className="mt-0.5 flex flex-col gap-0.5">
                {trace.search_queries.map((query) => (
                  <li key={query} className="truncate font-mono text-zinc-400" title={query}>
                    {query}
                  </li>
                ))}
              </ul>
            ) : null}
            {trace.search_results.length > 0 ? (
              <ul className="mt-1 flex flex-col gap-1">
                {trace.search_results.map((result) => (
                  <li key={`${result.query}-${result.url}`} className="text-zinc-400">
                    <span className="block truncate text-zinc-300" title={result.title}>
                      {result.title || result.url}
                    </span>
                    <span className="block truncate font-mono text-[11px]" title={result.url}>
                      {result.url}
                    </span>
                    {result.snippet ? (
                      <span className="block text-zinc-500">{result.snippet}</span>
                    ) : null}
                  </li>
                ))}
              </ul>
            ) : null}
            {trace.failures.length > 0 ? (
              <ul className="mt-1 flex flex-col gap-0.5 text-amber-300">
                {trace.failures.map((failure) => (
                  <li key={failure} className="truncate" title={failure}>
                    {failure}
                  </li>
                ))}
              </ul>
            ) : null}
          </div>
        ) : null}
        <div className="text-zinc-500">
          {trace.llm_ran
            ? "AI was used for this draft."
            : "Pre-filled from existing data — AI was not used."}
        </div>
      </div>
    </details>
  );
}

function ResearchAnnotation({
  annotation,
}: {
  annotation?: CrmResearchFieldAnnotation;
}) {
  if (!annotation) return null;
  const verifyUrl = `https://${annotation.source_domain}`;
  return (
    <div className="ml-24 flex max-w-xl flex-col gap-1 border-l border-emerald-900/70 pl-2 text-xs">
      <div className="flex flex-wrap items-center gap-2 text-zinc-400">
        <span className="rounded border border-emerald-800 bg-emerald-950/60 px-1.5 py-0.5 text-[11px] font-medium uppercase text-emerald-300">
          Research
        </span>
        <span className="rounded border border-zinc-700 px-1.5 py-0.5 text-[11px] uppercase text-zinc-300">
          {confidenceLabel(annotation.confidence)}
        </span>
        <span className="font-mono text-zinc-300">{annotation.source_domain}</span>
        <button
          type="button"
          onClick={() => window.open(verifyUrl, "_blank", "noopener,noreferrer")}
          className="text-sky-300 underline-offset-2 hover:text-sky-200 hover:underline"
        >
          Verify
        </button>
      </div>
      <div className="line-clamp-2 text-zinc-500" title={annotation.quote}>
        “{annotation.quote}”
      </div>
      {annotation.person_sensitive ? (
        <div className="text-amber-300">
          Found on {annotation.source_domain} — not verified to belong to this person.
          Confirm before saving.
        </div>
      ) : null}
    </div>
  );
}

/** Detail panel under an accepted queue row for the crm_record_create kind:
 * produce ONE draft proposing the CRM records the note references and that a
 * live CRM search found MISSING — a Company and/or a Contact. The operator
 * tunes which records to create and their fields, then approves (one
 * ensure-chain write, account → contact; dry-run while the gate is closed) or
 * rejects. Grounded names show their source quote. */
export default function CrmRecordDraftPanel({
  itemId,
  onUnauthorized,
}: {
  itemId: string;
  onUnauthorized: () => void;
}) {
  const {
    drafts,
    loaded,
    active: defaultActive,
    producing,
    notice,
    produce,
    runAction,
    load,
    busy,
    setNotice,
  } =
    useDraftPanel<CrmRecordDraftWithRevision>({
      itemId,
      produceKind: "crm_record_create",
      onUnauthorized,
      fetchDrafts: (id) => api.crmRecordDrafts(id),
      produceDraft: (req) => api.produceCrmRecordDraft(req),
      actionDraft: (draftId, req) => api.crmRecordDraftAction(draftId, req),
      produceTimeoutText:
        "The draft didn't finish after 3 minutes — either both records already exist or drafting failed (check AI Usage). Try again.",
    });

  const activeDrafts = useMemo(
    () => drafts.filter((entry) => entry.draft.status !== "rejected"),
    [drafts],
  );
  const [selectedDraftId, setSelectedDraftId] = useState<string | null>(null);
  const active =
    activeDrafts.find((entry) => entry.draft.draft_id === selectedDraftId) ??
    defaultActive;

  useEffect(() => {
    if (activeDrafts.length === 0) {
      setSelectedDraftId(null);
      return;
    }
    if (!activeDrafts.some((entry) => entry.draft.draft_id === selectedDraftId)) {
      setSelectedDraftId(activeDrafts[0].draft.draft_id);
    }
  }, [activeDrafts, selectedDraftId]);

  const [edit, setEdit] = useDraftEdit<CrmRecordDraftWithRevision, RecordEdit>(
    active,
    (entry) => ({
      create_company: entry.draft.create_company,
      company_name: orEmpty(entry.draft.company_name),
      company_website: orEmpty(entry.draft.company_website),
      company_phone: orEmpty(entry.draft.company_phone),
      company_address: orEmpty(entry.draft.company_address),
      company_description: orEmpty(entry.draft.company_description),
      create_contact: entry.draft.create_contact,
      contact_first_name: orEmpty(entry.draft.contact_first_name),
      contact_last_name: orEmpty(entry.draft.contact_last_name),
      contact_email: orEmpty(entry.draft.contact_email),
      contact_phone: orEmpty(entry.draft.contact_phone),
      contact_title: orEmpty(entry.draft.contact_title),
    }),
  );

  const dirty =
    active != null &&
    edit != null &&
    JSON.stringify(edit) !==
      JSON.stringify({
        create_company: active.draft.create_company,
        company_name: orEmpty(active.draft.company_name),
        company_website: orEmpty(active.draft.company_website),
        company_phone: orEmpty(active.draft.company_phone),
        company_address: orEmpty(active.draft.company_address),
        company_description: orEmpty(active.draft.company_description),
        create_contact: active.draft.create_contact,
        contact_first_name: orEmpty(active.draft.contact_first_name),
        contact_last_name: orEmpty(active.draft.contact_last_name),
        contact_email: orEmpty(active.draft.contact_email),
        contact_phone: orEmpty(active.draft.contact_phone),
        contact_title: orEmpty(active.draft.contact_title),
      });

  const quoteFor = (field: string) =>
    active?.draft.provenance.find((p) => p.field === field)?.quote ?? "";
  const researchAnnotationFor = (field: string) =>
    (
      active?.draft.research_annotations ??
      active?.draft.enrichment_trace?.research_annotations ??
      []
    ).find((annotation) => annotation.field_id === field);

  const [enrichmentRun, setEnrichmentRun] = useState<EnrichmentRun | null>(null);
  const [pendingEnrichment, setPendingEnrichment] = useState<{
    runId: string;
    alreadyRunning: boolean;
    mode: "standard" | "research";
  } | null>(null);
  const [domainSeed, setDomainSeed] = useState("");
  const draftLabel = (entry: CrmRecordDraftWithRevision) => {
    const contact = [
      entry.draft.contact_first_name,
      entry.draft.contact_last_name,
    ]
      .filter(Boolean)
      .join(" ");
    if (contact) return contact;
    if (entry.draft.company_name) return entry.draft.company_name;
    return entry.draft.draft_id;
  };

  const loadLatestEnrichmentRun = useCallback(async () => {
    if (!active) return null;
    const response = await api.enrichmentRuns({
      sliceId: "crm_record_drafts",
      draftId: active.draft.draft_id,
      limit: 1,
    });
    const run = response.runs[0] ?? null;
    setEnrichmentRun(run);
    return run;
  }, [active?.draft.draft_id]);

  useEffect(() => {
    if (!active) {
      setEnrichmentRun(null);
      setPendingEnrichment(null);
      return;
    }
    let cancelled = false;
    void loadLatestEnrichmentRun()
      .then((response) => {
        if (cancelled || !response) return;
        if (pendingEnrichment && isTerminalEnrichmentStatus(response.status)) {
          setPendingEnrichment(null);
        }
      })
      .catch((err: unknown) => {
        if (isUnauthorized(err)) onUnauthorized();
        if (!cancelled) setEnrichmentRun(null);
      });
    return () => {
      cancelled = true;
    };
  }, [active?.draft.draft_id, loadLatestEnrichmentRun, onUnauthorized]);

  useEffect(() => {
    if (!active || !pendingEnrichment) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const response = await api.enrichmentRuns({
          sliceId: "crm_record_drafts",
          draftId: active.draft.draft_id,
          limit: 3,
        });
        if (cancelled) return;
        const run =
          response.runs.find((entry) => entry.run_id === pendingEnrichment.runId) ??
          response.runs[0] ??
          null;
        setEnrichmentRun(run);
        if (run && isTerminalEnrichmentStatus(run.status)) {
          setPendingEnrichment(null);
          await load();
        }
      } catch (err) {
        if (isUnauthorized(err)) onUnauthorized();
      }
    };
    void tick();
    const id = setInterval(() => void tick(), 3_000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [active?.draft.draft_id, load, onUnauthorized, pendingEnrichment]);

  const runEnrichment = async (mode: "standard" | "research") => {
    if (!active) return;
    setNotice(null);
    try {
      const response = await api.enrichCrmRecordDraft(active.draft.draft_id, {
        idempotency_key: crypto.randomUUID(),
        domain_seed: domainSeed.trim() === "" ? null : domainSeed.trim(),
        ...(mode === "research" ? { mode: "research" as const } : {}),
      });
      setPendingEnrichment({
        runId: response.run_id,
        alreadyRunning: response.already_running,
        mode,
      });
      setEnrichmentRun(null);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice({ text: `Enrichment failed: ${errorMessage(err)}`, kind: "error" });
    }
  };

  return (
    <DraftPanelShell loaded={loaded} notice={notice}>
      {!active ? (
        <DraftEmptyCta
          message="No CRM records draft yet — draft one to find which referenced company/contact are missing from the CRM and propose creating them."
          buttonLabel="Propose CRM records"
          busyLabel="Searching CRM…"
          producing={producing}
          onProduce={() => void produce()}
          historyCount={drafts.length}
        />
      ) : (
        <div className="flex flex-col gap-2">
          {activeDrafts.length > 1 ? (
            <div className="flex flex-wrap gap-1">
              {activeDrafts.map((entry, index) => (
                <button
                  key={entry.draft.draft_id}
                  type="button"
                  onClick={() => setSelectedDraftId(entry.draft.draft_id)}
                  className={`h-7 rounded-md border px-2 text-xs ${
                    active?.draft.draft_id === entry.draft.draft_id
                      ? "border-sky-600 bg-sky-950/60 text-sky-100"
                      : "border-zinc-800 bg-zinc-950 text-zinc-400 hover:border-zinc-700 hover:text-zinc-200"
                  }`}
                  title={entry.draft.draft_id}
                >
                  {index + 1}. {draftLabel(entry)}
                </button>
              ))}
            </div>
          ) : null}

          <DraftStatusHeader
            status={active.draft.status}
            dryRun={active.outbox_job?.dry_run}
            confidence={active.draft.confidence}
            model={active.draft.model}
          />

          {active.draft.enrichment_trace ? (
            <EnrichmentTraceDetails trace={active.draft.enrichment_trace} />
          ) : null}

          {active.draft.status === "staged" ? (
            <div className="flex flex-wrap items-center gap-2 rounded-md border border-zinc-800 bg-zinc-950 px-2 py-1">
              <input
                value={domainSeed}
                onChange={(event) => setDomainSeed(event.target.value)}
                className="h-7 w-44 rounded-md border border-zinc-700 bg-zinc-950 px-2 text-xs text-zinc-200 placeholder:text-zinc-500 focus:border-sky-600 focus:outline-none disabled:opacity-40"
                placeholder="Domain (optional)"
                disabled={pendingEnrichment != null}
              />
              <Button
                variant="secondary"
                size="sm"
                busy={pendingEnrichment != null}
                onClick={() => void runEnrichment("standard")}
                title="Run web enrichment for this staged draft"
              >
                {pendingEnrichment ? "Enriching…" : "Enrich"}
              </Button>
              <Button
                variant="secondary"
                size="sm"
                busy={pendingEnrichment?.mode === "research"}
                disabled={pendingEnrichment != null}
                onClick={() => void runEnrichment("research")}
                title="Run bounded research for this staged draft"
              >
                Research
              </Button>
              <span className="min-w-0 flex-1 text-xs text-zinc-400">
                {pendingEnrichment
                  ? pendingEnrichment.alreadyRunning
                    ? "Already running…"
                    : pendingEnrichment.mode === "research"
                      ? "Researching…"
                      : "Enriching…"
                  : enrichmentRun
                    ? `Web search complete · ${enrichmentRun.proposals.length} suggestion${enrichmentRun.proposals.length === 1 ? "" : "s"} found`
                    : "No web search run yet"}
              </span>
            </div>
          ) : enrichmentRun ? (
            <div className="rounded-md border border-zinc-800 bg-zinc-950 px-2 py-1 text-xs text-zinc-400">
              Web search complete · {enrichmentRun.proposals.length}{" "}
              suggestion{enrichmentRun.proposals.length === 1 ? "" : "s"} found
            </div>
          ) : null}

          {active.draft.status === "staged" && edit ? (
            <div className="flex max-w-xl flex-col gap-3">
              <fieldset className="flex flex-col gap-2">
                <label className="flex cursor-pointer items-center gap-2 text-sm text-zinc-200">
                  <input
                    type="checkbox"
                    checked={edit.create_company}
                    onChange={(e) =>
                      setEdit({ ...edit, create_company: e.target.checked })
                    }
                    disabled={busy}
                    className="h-4 w-4 rounded border-zinc-600 bg-zinc-950 text-sky-600 focus:ring-1 focus:ring-sky-600"
                  />
                  Create company
                  {quoteFor("company_name") ? (
                    <span
                      className="text-xs italic text-zinc-500"
                      title="Source quote from the note"
                    >
                      "{quoteFor("company_name")}"
                    </span>
                  ) : null}
                </label>
                {edit.create_company ? (
                  <div className="flex flex-col gap-2 pl-6">
                    <DraftFieldInput
                      label="Company name"
                      quote=""
                      value={edit.company_name}
                      onChange={(company_name) =>
                        setEdit({ ...edit, company_name })
                      }
                      disabled={busy}
                    />
                    <ResearchAnnotation annotation={researchAnnotationFor("company_name")} />
                    <DraftFieldInput
                      label="Website"
                      quote={quoteFor("company_website")}
                      value={edit.company_website}
                      onChange={(company_website) =>
                        setEdit({ ...edit, company_website })
                      }
                      placeholder="none"
                      disabled={busy}
                    />
                    <ResearchAnnotation annotation={researchAnnotationFor("company_website")} />
                    <DraftFieldInput
                      label="Phone"
                      quote={quoteFor("company_phone")}
                      value={edit.company_phone}
                      onChange={(company_phone) =>
                        setEdit({ ...edit, company_phone })
                      }
                      placeholder="none"
                      disabled={busy}
                    />
                    <ResearchAnnotation annotation={researchAnnotationFor("company_phone")} />
                    <DraftFieldInput
                      label="Address"
                      quote={quoteFor("company_address")}
                      value={edit.company_address}
                      onChange={(company_address) =>
                        setEdit({ ...edit, company_address })
                      }
                      placeholder="none"
                      disabled={busy}
                    />
                    <ResearchAnnotation annotation={researchAnnotationFor("company_address")} />
                    <DraftFieldInput
                      label="Description"
                      quote={quoteFor("company_description")}
                      value={edit.company_description}
                      onChange={(company_description) =>
                        setEdit({ ...edit, company_description })
                      }
                      multiline
                      placeholder="none"
                      disabled={busy}
                    />
                    <ResearchAnnotation
                      annotation={researchAnnotationFor("company_description")}
                    />
                  </div>
                ) : null}
              </fieldset>

              <fieldset className="flex flex-col gap-2">
                <label className="flex cursor-pointer items-center gap-2 text-sm text-zinc-200">
                  <input
                    type="checkbox"
                    checked={edit.create_contact}
                    onChange={(e) =>
                      setEdit({ ...edit, create_contact: e.target.checked })
                    }
                    disabled={busy}
                    className="h-4 w-4 rounded border-zinc-600 bg-zinc-950 text-sky-600 focus:ring-1 focus:ring-sky-600"
                  />
                  Create contact
                  {quoteFor("contact_name") ? (
                    <span
                      className="text-xs italic text-zinc-500"
                      title="Source quote from the note"
                    >
                      "{quoteFor("contact_name")}"
                    </span>
                  ) : null}
                </label>
                {edit.create_contact ? (
                  <div className="flex flex-col gap-2 pl-6">
                    <DraftFieldInput
                      label="First name"
                      quote=""
                      value={edit.contact_first_name}
                      onChange={(contact_first_name) =>
                        setEdit({ ...edit, contact_first_name })
                      }
                      disabled={busy}
                    />
                    <DraftFieldInput
                      label="Last name"
                      quote=""
                      value={edit.contact_last_name}
                      onChange={(contact_last_name) =>
                        setEdit({ ...edit, contact_last_name })
                      }
                      disabled={busy}
                    />
                    <DraftFieldInput
                      label="Email"
                      quote={quoteFor("contact_email")}
                      value={edit.contact_email}
                      onChange={(contact_email) =>
                        setEdit({ ...edit, contact_email })
                      }
                      placeholder="none"
                      disabled={busy}
                    />
                    <ResearchAnnotation annotation={researchAnnotationFor("contact_email")} />
                    <DraftFieldInput
                      label="Phone"
                      quote={quoteFor("contact_phone")}
                      value={edit.contact_phone}
                      onChange={(contact_phone) =>
                        setEdit({ ...edit, contact_phone })
                      }
                      placeholder="none"
                      disabled={busy}
                    />
                    <ResearchAnnotation annotation={researchAnnotationFor("contact_phone")} />
                    <DraftFieldInput
                      label="Title"
                      quote={quoteFor("contact_title")}
                      value={edit.contact_title}
                      onChange={(contact_title) =>
                        setEdit({ ...edit, contact_title })
                      }
                      placeholder="none"
                      disabled={busy}
                    />
                    <ResearchAnnotation annotation={researchAnnotationFor("contact_title")} />
                  </div>
                ) : null}
              </fieldset>
            </div>
          ) : (
            <div className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
              {(
                [
                  [
                    "Company",
                    active.draft.create_company
                      ? `Create: ${active.draft.company_name ?? "—"}`
                      : active.draft.company_name
                        ? `Matched: ${active.draft.company_name}`
                        : "—",
                  ],
                  [
                    "Contact",
                    active.draft.create_contact
                      ? `Create: ${[active.draft.contact_first_name, active.draft.contact_last_name].filter(Boolean).join(" ") || "—"}`
                      : "—",
                  ],
                ] as const
              ).map(([label, value]) => (
                <div key={label} className="contents">
                  <span className="text-zinc-400">{label}</span>
                  <span className="text-zinc-200">{value}</span>
                </div>
              ))}
            </div>
          )}

          <DraftActionFooter
            visible={active.draft.status === "staged"}
            busy={busy}
            dirty={dirty}
            approveLabel="Approve → CRM"
            approveDirtyLabel="Save & approve → CRM"
            approveTitle="Creates the company and contact records in your CRM when you approve."
            onApprove={() =>
              void runAction(
                active,
                "approve",
                dirty && edit
                  ? async (revision) => {
                      const saved = await api.updateCrmRecordDraft(
                        active.draft.draft_id,
                        {
                          create_company: edit.create_company,
                          company_name: orNull(edit.company_name),
                          company_website: orNull(edit.company_website),
                          company_phone: orNull(edit.company_phone),
                          company_address: orNull(edit.company_address),
                          company_description: orNull(edit.company_description),
                          create_contact: edit.create_contact,
                          contact_first_name: orNull(edit.contact_first_name),
                          contact_last_name: orNull(edit.contact_last_name),
                          contact_email: orNull(edit.contact_email),
                          contact_phone: orNull(edit.contact_phone),
                          contact_title: orNull(edit.contact_title),
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
                create_company: active.draft.create_company,
                company_name: orEmpty(active.draft.company_name),
                company_website: orEmpty(active.draft.company_website),
                company_phone: orEmpty(active.draft.company_phone),
                company_address: orEmpty(active.draft.company_address),
                company_description: orEmpty(active.draft.company_description),
                create_contact: active.draft.create_contact,
                contact_first_name: orEmpty(active.draft.contact_first_name),
                contact_last_name: orEmpty(active.draft.contact_last_name),
                contact_email: orEmpty(active.draft.contact_email),
                contact_phone: orEmpty(active.draft.contact_phone),
                contact_title: orEmpty(active.draft.contact_title),
              })
            }
          />

          <OutboxStateLine
            job={active.outbox_job}
            show={active.draft.status === "approved"}
            dryRunText="Tested successfully, but live CRM writes are turned off — ask your administrator to enable them."
            deliveredText={() => "Records created in the CRM"}
            onUnauthorized={onUnauthorized}
            onRetried={load}
          />
        </div>
      )}
    </DraftPanelShell>
  );
}
