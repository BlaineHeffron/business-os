import { useCallback, useEffect, useState } from "react";
import type { ConnectorStatus } from "../../types/generated/ConnectorStatus";
import { api, errorMessage, isUnauthorized } from "../../lib/api";
import { Button, Card } from "../../components/ui";

function scopeLabel(scope: string): string {
  return scope.replace("https://www.googleapis.com/auth/", "");
}

export function GoogleSettings({
  onUnauthorized,
  onChanged,
}: {
  onUnauthorized: () => void;
  onChanged?: () => void;
}) {
  const [status, setStatus] = useState<ConnectorStatus | null>(null);
  const [loading, setLoading] = useState(false);
  const [disconnecting, setDisconnecting] = useState(false);
  const [notice, setNotice] = useState<{ kind: "ok" | "error"; text: string } | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const next = await api.connectorStatus();
      setStatus(next);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      setNotice({ kind: "error", text: `Failed to load Google status: ${errorMessage(err)}` });
    } finally {
      setLoading(false);
    }
  }, [onUnauthorized]);

  useEffect(() => {
    void load();
  }, [load]);

  const connect = () => {
    const connectUrl = status?.connect_url ?? null;
    if (!connectUrl) return;
    window.open(connectUrl, "_blank", "noopener");
  };

  const disconnect = async () => {
    if (!status?.connected) return;
    const confirmed = window.confirm(
      "Disconnect this browser user's Google account from BusinessOS?",
    );
    if (!confirmed) return;
    setDisconnecting(true);
    setNotice(null);
    try {
      await api.disconnectGoogle();
      setNotice({ kind: "ok", text: "Google account disconnected." });
      await load();
      onChanged?.();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      setNotice({ kind: "error", text: errorMessage(err) });
    } finally {
      setDisconnecting(false);
    }
  };

  const missingScopes = status?.missing_scopes ?? [];
  const scopes = status?.scopes ?? [];
  const canConnect = Boolean(status?.connect_url);
  const canDisconnect = Boolean(status?.connected && status.source === "stored");

  return (
    <Card className="space-y-4 p-4">
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <h3 className="text-sm font-semibold text-zinc-100">Google account</h3>
          <p className="mt-1 text-sm text-zinc-400">
            {status?.connected
              ? `Connected${status.source ? ` from ${status.source}` : ""}.`
              : status?.blocked_reason
                ? "Google OAuth is not configured for this instance."
                : "No Google account is connected for this user."}
          </p>
        </div>
        <div className="flex flex-wrap justify-start gap-2 sm:justify-end">
          <Button variant="secondary" size="sm" busy={loading} onClick={() => void load()}>
            Refresh
          </Button>
          {canConnect ? (
            <Button variant="primary" size="sm" onClick={connect}>
              {status?.connected ? "Reconnect Google" : "Connect Google"}
            </Button>
          ) : null}
          {canDisconnect ? (
            <Button
              variant="danger"
              size="sm"
              busy={disconnecting}
              onClick={() => void disconnect()}
            >
              Disconnect
            </Button>
          ) : null}
        </div>
      </div>

      {notice ? (
        <div
          className={`rounded-md border px-3 py-2 text-sm ${
            notice.kind === "ok"
              ? "border-emerald-900/60 bg-emerald-950/30 text-emerald-200"
              : "border-red-900/60 bg-red-950/30 text-red-200"
          }`}
        >
          {notice.text}
        </div>
      ) : null}

      {missingScopes.length > 0 ? (
        <div className="rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-2 text-sm text-amber-200">
          Missing scopes: {missingScopes.map(scopeLabel).join(", ")}
        </div>
      ) : null}

      {scopes.length > 0 ? (
        <div>
          <div className="mb-2 text-xs font-semibold uppercase tracking-wide text-zinc-500">
            Granted scopes
          </div>
          <div className="flex flex-wrap gap-2">
            {scopes.map((scope) => (
              <span
                key={scope}
                className="rounded-full border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs text-zinc-300"
              >
                {scopeLabel(scope)}
              </span>
            ))}
          </div>
        </div>
      ) : null}
    </Card>
  );
}
