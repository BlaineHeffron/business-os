import type { ConnectorStatus } from "../types/generated/ConnectorStatus";
import { Button } from "./ui";

export default function ConnectorBanner({
  status,
}: {
  status: ConnectorStatus | null;
}) {
  if (!status) return null;

  if (status.connected) {
    // Connected but consented before a scope was added (e.g. calendar.events
    // for the approval → calendar write path): prompt a reconnect.
    const missingScopes = status.missing_scopes ?? [];
    const connectUrl = status.connect_url;
    if (missingScopes.length === 0 || !connectUrl) return null;
    const reconnect = () => {
      window.open(connectUrl, "_blank", "noopener");
    };
    return (
      <div className="mb-4 flex items-center justify-between gap-3 rounded-lg border border-amber-900/60 bg-amber-950/30 px-4 py-3 text-sm">
        <div className="text-amber-200">
          <span className="font-semibold">Google needs to be reconnected.</span>{" "}
          Reconnect to grant calendar access so approved events can be saved.
        </div>
        <Button variant="primary" size="sm" onClick={reconnect} className="shrink-0">
          Reconnect Google
        </Button>
      </div>
    );
  }

  if (status.blocked_reason) {
    return (
      <div className="mb-4 flex items-center gap-3 rounded-lg border border-red-900/60 bg-red-950/40 px-4 py-3 text-sm text-red-300">
        <span className="font-semibold">
          Google needs attention.
        </span>
        <span>Ask your administrator to review the connection.</span>
      </div>
    );
  }

  const openConnect = () => {
    if (!status.connect_url) return;
    window.open(status.connect_url, "_blank", "noopener");
  };

  return (
    <div className="mb-4 flex items-center justify-between gap-3 rounded-lg border border-amber-900/60 bg-amber-950/30 px-4 py-3 text-sm">
      <div className="text-amber-200">
        <span className="font-semibold">Google is not connected.</span> New email
        won&apos;t arrive until Google is connected.
      </div>
      {status.connect_url ? (
        <Button variant="primary" size="sm" onClick={openConnect} className="shrink-0">
          Connect Google
        </Button>
      ) : null}
    </div>
  );
}
