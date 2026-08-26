import { useEffect, useState } from "react";
import type { CrmContactSnapshot } from "../types/generated/CrmContactSnapshot";
import type { CrmDealSnapshot } from "../types/generated/CrmDealSnapshot";
import { api, isUnauthorized } from "../lib/api";

type CrmContextState =
  | { status: "idle" | "loading" | "empty" | "error" }
  | {
      status: "ready";
      contacts: CrmContactSnapshot[];
      deals: CrmDealSnapshot[];
      lookupEmail: string | null;
      hubspotLinksConfigured: boolean;
    };

export function extractEmailAddress(raw: string | null | undefined): string | null {
  const value = raw?.trim() ?? "";
  if (!value) return null;
  const match = value.match(/<([^>]+)>/);
  const email = (match ? match[1] : value).trim().toLowerCase();
  return email.includes("@") ? email : null;
}

export default function CrmContextLinks({
  sourceKey,
  rawAddress,
  onUnauthorized,
}: {
  sourceKey: string;
  rawAddress: string | null | undefined;
  onUnauthorized: () => void;
}) {
  const [state, setState] = useState<CrmContextState>({ status: "idle" });

  useEffect(() => {
    if (!sourceKey.trim()) {
      setState({ status: "empty" });
      return;
    }
    let alive = true;
    setState({ status: "loading" });
    void (async () => {
      try {
        const context = await api.crmCacheContext(sourceKey);
        if (!alive) return;
        if (
          context.contacts.length === 0 &&
          context.deals.length === 0
        ) {
          setState({ status: "empty" });
          return;
        }
        if (alive) {
          setState({
            status: "ready",
            contacts: context.contacts,
            deals: context.deals,
            lookupEmail: context.lookup_email ?? extractEmailAddress(rawAddress),
            hubspotLinksConfigured: context.hubspot_links_configured,
          });
        }
      } catch (err) {
        if (isUnauthorized(err)) onUnauthorized();
        else if (alive) setState({ status: "error" });
      }
    })();
    return () => {
      alive = false;
    };
  }, [sourceKey, rawAddress, onUnauthorized]);

  if (state.status !== "ready") return null;

  return (
    <section className="mt-3 border-t border-zinc-800 pt-3">
      <div className="mb-2 flex items-center justify-between gap-2">
        <div className="text-xs font-semibold text-zinc-200">CRM</div>
        <div className="text-[11px] text-zinc-500">{state.lookupEmail}</div>
      </div>
      {!state.hubspotLinksConfigured ? (
        <div className="mb-2 rounded border border-amber-500/30 bg-amber-500/10 px-2 py-1 text-xs text-amber-100">
          HubSpot links need the portal ID in Settings.
        </div>
      ) : null}
      <div className="space-y-2">
        {state.contacts.map((contact) => (
          <div
            key={contact.provider_contact_id}
            className="rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2"
          >
            <div className="flex min-w-0 items-start justify-between gap-2">
              <div className="min-w-0">
                <div className="truncate text-sm font-medium text-zinc-100">
                  {contact.name ?? contact.email ?? contact.provider_contact_id}
                </div>
                <div className="mt-0.5 flex flex-wrap gap-x-2 gap-y-0.5 text-xs text-zinc-400">
                  {contact.company ? <span>{contact.company}</span> : null}
                  {contact.lifecycle_stage ? <span>{contact.lifecycle_stage}</span> : null}
                  {contact.owner ? <span>{contact.owner}</span> : null}
                </div>
              </div>
              {contact.contact_url ? (
                <a
                  href={contact.contact_url}
                  target="_blank"
                  rel="noreferrer"
                  className="shrink-0 rounded-md border border-zinc-700 px-2 py-1 text-xs font-medium text-zinc-300 hover:bg-zinc-800 hover:text-zinc-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70"
                >
                  Open in CRM
                </a>
              ) : null}
            </div>
          </div>
        ))}
      </div>
      {state.deals.length > 0 ? (
        <div className="mt-2 space-y-2">
          {state.deals.map((deal) => (
            <div
              key={deal.provider_deal_id}
              className="rounded-md border border-zinc-800 bg-zinc-950 px-3 py-2"
            >
              <div className="flex min-w-0 items-start justify-between gap-2">
                <div className="min-w-0">
                  <div className="truncate text-sm font-medium text-zinc-100">
                    {deal.name ?? deal.provider_deal_id}
                  </div>
                  <div className="mt-0.5 flex flex-wrap gap-x-2 gap-y-0.5 text-xs text-zinc-400">
                    {deal.stage ? <span>Stage: {deal.stage}</span> : null}
                    {deal.pipeline ? <span>Pipeline: {deal.pipeline}</span> : null}
                    {deal.close_date ? <span>Close: {deal.close_date}</span> : null}
                    {deal.amount_visible && deal.amount_cents !== null && deal.amount_cents !== undefined ? (
                      <span>{formatMoney(deal.amount_cents, deal.currency)}</span>
                    ) : null}
                  </div>
                </div>
                {deal.deal_url ? (
                  <a
                    href={deal.deal_url}
                    target="_blank"
                    rel="noreferrer"
                    className="shrink-0 rounded-md border border-zinc-700 px-2 py-1 text-xs font-medium text-zinc-300 hover:bg-zinc-800 hover:text-zinc-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70"
                  >
                    Open deal
                  </a>
                ) : null}
              </div>
            </div>
          ))}
        </div>
      ) : null}
    </section>
  );
}

function formatMoney(cents: number, currency: string | null | undefined): string {
  return new Intl.NumberFormat(undefined, {
    style: "currency",
    currency: currency ?? "USD",
  }).format(cents / 100);
}
