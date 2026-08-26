import { useEffect, useState } from "react";
import type { WorkItemSourceResponse } from "../types/generated/WorkItemSourceResponse";
import { api, errorMessage, isUnauthorized } from "../lib/api";
import CrmContextLinks from "./CrmContextLinks";
import EmailBodyPreview from "./EmailBodyPreview";

/**
 * Inline view of the full source behind a work item (email or note) —
 * the decision happens in the feed, never behind a navigation.
 */
export default function SourcePeek({
  itemId,
  onUnauthorized,
}: {
  itemId: string;
  onUnauthorized: () => void;
}) {
  const [source, setSource] = useState<WorkItemSourceResponse | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    api
      .workItemSource(itemId)
      .then((res) => {
        if (alive) setSource(res);
      })
      .catch((err: unknown) => {
        if (isUnauthorized(err)) onUnauthorized();
        else if (alive) setError(errorMessage(err));
      });
    return () => {
      alive = false;
    };
  }, [itemId, onUnauthorized]);

  if (error) {
    return (
      <div className="border-t border-zinc-800/80 bg-zinc-950/60 px-4 py-3 text-xs text-red-300">
        Failed to load source: {error}
      </div>
    );
  }
  if (source === null) {
    return (
      <div className="border-t border-zinc-800/80 bg-zinc-950/60 px-4 py-3 text-xs text-zinc-400">
        Loading source…
      </div>
    );
  }

  const msg = source.message;
  const headerRows: [string, string][] = [];
  if (msg.from_addr) headerRows.push(["From", msg.from_addr]);
  if (msg.to_addr) headerRows.push(["To", msg.to_addr]);
  if (msg.subject) headerRows.push(["Subject", msg.subject]);
  if (msg.internal_date_ms !== null) {
    headerRows.push(["Date", new Date(Number(msg.internal_date_ms)).toLocaleString()]);
  }

  return (
    <div className="border-t border-zinc-800/80 bg-zinc-950/60 px-4 py-3">
      <div className="mb-2 text-xs font-semibold text-zinc-200">
        {source.source_kind === "operator_note" ? "Logged note" : "Source email"}
      </div>
      {headerRows.length > 0 ? (
        <dl className="mb-2 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 text-xs">
          {headerRows.map(([label, value]) => (
            <div key={label} className="contents">
              <dt className="text-zinc-400">{label}</dt>
              <dd className="min-w-0 break-words text-zinc-300">{value}</dd>
            </div>
          ))}
        </dl>
      ) : null}
      <SourceBody source={source} />
      <CrmContextLinks
        sourceKey={msg.source_key}
        rawAddress={msg.from_addr}
        onUnauthorized={onUnauthorized}
      />
      {msg.attachments.length > 0 ? (
        <div className="mt-3 border-t border-zinc-800 pt-3">
          <div className="mb-1 text-xs font-semibold text-zinc-200">Attachments</div>
          <div className="flex flex-wrap gap-2">
            {msg.attachments.map((attachment) => (
              <span
                key={attachment.attachment_id}
                className="max-w-full rounded border border-zinc-700 bg-zinc-900 px-2 py-1 text-xs text-zinc-300"
                title={attachment.attachment_id}
              >
                {attachment.filename}
                <span className="text-zinc-500">
                  {" "}
                  {attachment.mime_type ?? "unknown"} ·{" "}
                  {formatBytes(attachment.size_bytes)}
                </span>
              </span>
            ))}
          </div>
        </div>
      ) : null}
    </div>
  );
}

function SourceBody({ source }: { source: WorkItemSourceResponse }) {
  const body = source.source_body || source.message.body_excerpt;
  return <EmailBodyPreview body={body} format={source.source_body_format} />;
}

function formatBytes(bytes: number | null): string {
  if (bytes === null) return "unknown size";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
