import { useCallback, useEffect, useState } from "react";
import { useAppCommand } from "../lib/commands";
import type { CalendarOption } from "../types/generated/CalendarOption";
import type { OperatorUser } from "../types/generated/OperatorUser";
import { api, errorMessage, isUnauthorized } from "../lib/api";
import DriveCorpusCard from "../components/DriveCorpusCard";
import SectionHelpButton from "../components/SectionHelpButton";
import {
  Button,
  ConfirmDialog,
  EmptyState,
  SkeletonRows,
  StatusBadge,
} from "../components/ui";

/** Operator users: named logins with personal bearer tokens. Tokens appear
 * exactly once (create / rotate) — copy them then; they are never readable
 * again. Each person signs in by pasting their token under Settings. */
export default function Users({
  onUnauthorized,
  helpTopicId,
  onOpenHelpTopic,
}: {
  onUnauthorized: () => void;
  helpTopicId?: string;
  onOpenHelpTopic: (topicId: string) => void;
}) {
  const [users, setUsers] = useState<OperatorUser[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [newName, setNewName] = useState("");
  const [showArchived, setShowArchived] = useState(false);
  /** (user_id, token) revealed by the last create/rotate — shown once. */
  const [revealed, setRevealed] = useState<{
    userId: string;
    token: string;
  } | null>(null);
  const [confirmDisable, setConfirmDisable] = useState<OperatorUser | null>(
    null,
  );
  const [confirmArchive, setConfirmArchive] = useState<OperatorUser | null>(
    null,
  );
  // My identity + my writable calendars: the default-calendar picker only
  // renders on YOUR row (it lists the calendars YOUR credential can write).
  const [meId, setMeId] = useState<string | null>(null);
  const [calendars, setCalendars] = useState<CalendarOption[] | null>(null);

  const load = useCallback(async () => {
    try {
      const res = await api.users(showArchived);
      setUsers(res.users);
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice(`Failed to load users: ${errorMessage(err)}`);
    } finally {
      setLoaded(true);
    }
  }, [onUnauthorized, showArchived]);

  useAppCommand("refresh", () => void load());

  useEffect(() => {
    void load();
    void (async () => {
      try {
        const me = await api.whoami();
        setMeId(me.actor_id);
        if (me.actor_id !== "operator") {
          const listed = await api.calendarOptions();
          setCalendars(listed.calendars);
        }
      } catch {
        // No connected Google account (or shared identity): no picker.
      }
    })();
  }, [load]);

  const run = async (work: () => Promise<void>) => {
    setBusy(true);
    setNotice(null);
    try {
      await work();
      await load();
    } catch (err) {
      if (isUnauthorized(err)) onUnauthorized();
      else setNotice(errorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex max-w-2xl flex-col gap-4">
      <div className="surface-section-head surface-head-emerald">
        <div className="min-w-0 flex-1">
          <div className="flex min-w-0 items-center gap-2">
            <h2 className="shrink-0 whitespace-nowrap text-lg font-semibold text-zinc-100">
              Operator users
            </h2>
            <SectionHelpButton
              topicId={helpTopicId}
              onOpenHelp={onOpenHelpTopic}
              label="Open help for Users"
            />
          </div>
          <p className="mt-1 text-sm text-zinc-400">
            Each person gets their own sign-in token, so approvals and edits are
            recorded under their name. Tokens are shown <strong>once</strong> —
            copy them immediately. Sign in by pasting the token under Settings.
          </p>
        </div>
      </div>

      {notice ? (
        <div className="rounded-md border border-amber-900/60 bg-amber-950/30 px-3 py-2 text-sm text-amber-300">
          {notice}
        </div>
      ) : null}

      {revealed ? (
        <div className="rounded-lg border border-emerald-800/70 bg-emerald-950/30 p-4">
          <div className="text-sm font-semibold text-emerald-300">
            Token for {revealed.userId} — copy it now, it will not be shown
            again
          </div>
          <div className="mt-2 flex items-center gap-2">
            <code className="flex-1 break-all rounded-md bg-zinc-950 px-3 py-2 font-mono text-xs text-emerald-200">
              {revealed.token}
            </code>
            <Button
              variant="secondary"
              size="sm"
              onClick={() =>
                void navigator.clipboard.writeText(revealed.token)
              }
              className="border-emerald-800 text-emerald-300 hover:bg-emerald-900/40"
            >
              Copy
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={() => setRevealed(null)}
            >
              Dismiss
            </Button>
          </div>
        </div>
      ) : null}

      <div className="surface-card surface-flat surface-body-emerald flex items-center gap-2 rounded-lg border border-zinc-800 p-3">
        <input
          value={newName}
          onChange={(e) => setNewName(e.target.value)}
          placeholder="Display name (e.g. Jordan)"
          className="flex-1 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-1.5 text-sm text-zinc-200 placeholder:text-zinc-500 focus:border-sky-600 focus:outline-none"
        />
        <Button
          variant="primary"
          size="sm"
          disabled={busy || newName.trim().length === 0}
          onClick={() =>
            void run(async () => {
              const res = await api.createUser({
                display_name: newName.trim(),
                idempotency_key: crypto.randomUUID(),
                actor_id: null,
              });
              setRevealed({ userId: res.user.user_id, token: res.token });
              setNewName("");
            })
          }
        >
          Add user
        </Button>
      </div>

      <label className="flex items-center gap-2 text-xs text-zinc-400">
        <input
          type="checkbox"
          checked={showArchived}
          onChange={(event) => setShowArchived(event.target.checked)}
          className="h-3.5 w-3.5 rounded border-zinc-700 bg-zinc-950"
        />
        Show archived users
      </label>

      {!loaded ? (
        <div className="surface-card surface-flat surface-body-emerald surface-row-divide divide-y divide-zinc-800/80 rounded-lg border border-zinc-800">
          <table className="w-full">
            <tbody>
              <SkeletonRows rows={3} cols={3} />
            </tbody>
          </table>
        </div>
      ) : users.length === 0 ? (
        <EmptyState title="No users yet.">
          Everyone shares the anonymous &ldquo;operator&rdquo; identity until
          you add them.
        </EmptyState>
      ) : (
        <div className="surface-card surface-flat surface-body-emerald surface-row-divide divide-y divide-zinc-800/80 rounded-lg border border-zinc-800">
          {users.map((user) => (
            <div
              key={user.user_id}
              className="flex items-center gap-3 px-3 py-2.5"
            >
              <div className="min-w-0 flex-1">
                <span className="text-sm font-semibold text-zinc-100">
                  {user.display_name}
                </span>
                <span className="ml-2 font-mono text-xs text-zinc-400">
                  {user.user_id}
                </span>
                {!user.active ? (
                  <span className="ml-2">
                    <StatusBadge tone="critical">disabled</StatusBadge>
                  </span>
                ) : null}
                {user.archived_at_ms !== null ? (
                  <span className="ml-2">
                    <StatusBadge tone="neutral">archived</StatusBadge>
                  </span>
                ) : null}
              </div>
              {user.user_id === meId && calendars !== null ? (
                <select
                  disabled={busy}
                  value={user.default_calendar_id ?? ""}
                  onChange={(e) => {
                    const picked = e.target.value;
                    void run(async () => {
                      await api.setUserDefaultCalendar(user.user_id, {
                        calendar_id: picked === "" ? null : picked,
                        expected_revision: null,
                        idempotency_key: crypto.randomUUID(),
                        actor_id: null,
                      });
                    });
                  }}
                  className="max-w-48 rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1 text-xs text-zinc-300 focus:border-sky-600 focus:outline-none"
                  title="Where your approved event drafts go when the draft doesn't pick a calendar"
                >
                  <option value="">Default calendar: server setting</option>
                  {calendars.map((cal) => (
                    <option key={cal.id} value={cal.id}>
                      {cal.summary}
                      {cal.primary ? " (primary)" : ""}
                    </option>
                  ))}
                </select>
              ) : user.default_calendar_id ? (
                <span
                  className="max-w-48 truncate text-xs text-zinc-400"
                  title={`Default calendar: ${user.default_calendar_id}`}
                >
                  cal: {user.default_calendar_id}
                </span>
              ) : null}
              {user.archived_at_ms === null ? (
                <Button
                  variant="secondary"
                  size="sm"
                  disabled={busy}
                  onClick={() =>
                    void run(async () => {
                      const res = await api.rotateUserToken(user.user_id, {
                        idempotency_key: crypto.randomUUID(),
                        actor_id: null,
                      });
                      setRevealed({ userId: user.user_id, token: res.token });
                    })
                  }
                  title="Issue a replacement token (the old one stops working)"
                >
                  Rotate token
                </Button>
              ) : null}
              {user.archived_at_ms !== null ? null : user.active ? (
                <Button
                  variant="danger"
                  size="sm"
                  disabled={busy}
                  onClick={() => setConfirmDisable(user)}
                  title="Disable — their token stops working immediately"
                >
                  Disable
                </Button>
              ) : (
                <>
                  <Button
                    variant="secondary"
                    size="sm"
                    disabled={busy}
                    onClick={() =>
                      void run(async () => {
                        await api.userAction(user.user_id, {
                          action: "enable",
                          expected_revision: null,
                          idempotency_key: crypto.randomUUID(),
                          actor_id: null,
                        });
                      })
                    }
                  >
                    Enable
                  </Button>
                  {user.user_id !== meId ? (
                    <Button
                      variant="danger"
                      size="sm"
                      disabled={busy}
                      onClick={() => setConfirmArchive(user)}
                      title="Archive — hides this disabled user from the normal list"
                    >
                      Archive
                    </Button>
                  ) : null}
                </>
              )}
            </div>
          ))}
        </div>
      )}

      <ConfirmDialog
        open={confirmDisable !== null}
        title={`Disable ${confirmDisable?.display_name ?? "user"}?`}
        body="Their token stops working immediately. You can re-enable them at any time."
        confirmLabel="Disable user"
        busy={busy}
        onCancel={() => setConfirmDisable(null)}
        onConfirm={() => {
          if (!confirmDisable) return;
          const target = confirmDisable;
          setConfirmDisable(null);
          void run(async () => {
            await api.userAction(target.user_id, {
              action: "disable",
              expected_revision: null,
              idempotency_key: crypto.randomUUID(),
              actor_id: null,
            });
          });
        }}
      />

      <ConfirmDialog
        open={confirmArchive !== null}
        title={`Archive ${confirmArchive?.display_name ?? "user"}?`}
        body="This hides the disabled user from the normal Users view while preserving audit history. Disconnect linked credentials first."
        confirmLabel="Archive user"
        busy={busy}
        onCancel={() => setConfirmArchive(null)}
        onConfirm={() => {
          if (!confirmArchive) return;
          const target = confirmArchive;
          setConfirmArchive(null);
          void run(async () => {
            await api.userAction(target.user_id, {
              action: "archive",
              expected_revision: null,
              idempotency_key: crypto.randomUUID(),
              actor_id: null,
            });
          });
        }}
      />

      <DriveCorpusCard onUnauthorized={onUnauthorized} />
    </div>
  );
}
