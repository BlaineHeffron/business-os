type EmailBodyPreviewFormat = "plain_text" | "html";

export function detectEmailBodyFormat(body: string): EmailBodyPreviewFormat {
  return looksLikeHtml(body) ? "html" : "plain_text";
}

export default function EmailBodyPreview({
  body,
  format,
  className = "",
}: {
  body: string;
  format: EmailBodyPreviewFormat;
  className?: string;
}) {
  if (!body) {
    return <div className="text-xs italic text-zinc-400">(empty)</div>;
  }
  if (format === "html") {
    return (
      <iframe
        title="Email body"
        sandbox=""
        srcDoc={htmlPreviewDocument(body)}
        className={`h-96 w-full rounded-md border border-zinc-800 bg-white ${className}`}
      />
    );
  }
  return (
    <div
      className={`whitespace-pre-wrap break-words text-xs leading-relaxed text-zinc-300 ${className}`}
    >
      {body}
    </div>
  );
}

function htmlPreviewDocument(body: string): string {
  return `<!doctype html>
<html>
<head>
  <meta charset="utf-8" />
  <meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data: cid:; style-src 'unsafe-inline'; font-src data:; base-uri 'none'; form-action 'none'" />
  <base target="_blank" />
  <style>
    html, body { margin: 0; padding: 0; background: #fff; color: #18181b; }
    body { box-sizing: border-box; padding: 16px; font: 14px/1.5 -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; overflow-wrap: anywhere; }
    table { max-width: 100%; }
    img { max-width: 100%; height: auto; }
    pre { white-space: pre-wrap; }
  </style>
</head>
<body>${body}</body>
</html>`;
}

function looksLikeHtml(body: string): boolean {
  const lowerHead = body.trimStart().slice(0, 512).toLowerCase();
  return (
    lowerHead.startsWith("<!doctype html") ||
    lowerHead.startsWith("<html") ||
    lowerHead.startsWith("<body") ||
    lowerHead.includes("<div") ||
    lowerHead.includes("<table") ||
    lowerHead.includes("<p") ||
    lowerHead.includes("<br") ||
    lowerHead.includes("<span")
  );
}
