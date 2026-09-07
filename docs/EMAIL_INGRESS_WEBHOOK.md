# Email ingress webhook

Authenticated inbound HTTP webhook that opens a work item through the existing email triage → work queue path **without** an LLM classification call.

Gmail OAuth ingest is unchanged. This is the durable home for already-filtered events (Apps Script, monitors, forms).

## Setup

1. Set `BOS_EMAIL_INGRESS_WEBHOOK_SECRET` on the BusinessOS host. Unset = the route 404s (fail closed).
2. `POST /api/webhooks/email-ingress`
3. Auth (either):
   - `Authorization: Bearer <secret>`
   - `X-Bos-Hook-Token: <secret>`
4. Operator tokens are rejected. Missing/wrong secret → 401.

Give the target triage category a work-queue policy with `create_work_item` and at least one packet kind. Without that policy, the message is stored and AI is skipped, but no work item is opened.

An Apps Script POST example lives in [`examples/email-ingress-webhook/`](../examples/email-ingress-webhook/).

## curl

```bash
curl -sS -X POST "$BOS_URL/api/webhooks/email-ingress" \
  -H "Authorization: Bearer $BOS_EMAIL_INGRESS_WEBHOOK_SECRET" \
  -H "Content-Type: application/json" \
  -d '{
    "source": "business-os.gmail-webhook-trigger",
    "version": 1,
    "receivedAt": "2026-09-07T15:00:00.000Z",
    "query": "from:sineadpatience@me.com newer_than:14d -label:bos-webhooked",
    "message": {
      "messageId": "msg-1",
      "threadId": "thr-1",
      "from": "Sinead <sineadpatience@me.com>",
      "to": "ops@example.com",
      "subject": "Site change",
      "snippet": "The homepage copy changed.",
      "date": "2026-09-07T15:00:00.000Z"
    }
  }'
```

## Payload

Canonical body is the nested Apps Script shape from issue #12 / PR #13 (`examples/gmail-webhook-trigger/Code.gs` and `examples/email-ingress-webhook/Code.gs`). Extra fields (`version`, `query`) are ignored. Optional top-level `ruleId` / `clientId` / `body` are accepted alongside `snippet`.

```json
{
  "source": "business-os.gmail-webhook-trigger",
  "version": 1,
  "receivedAt": "2026-09-07T15:00:00.000Z",
  "query": "from:sineadpatience@me.com newer_than:14d -label:bos-webhooked",
  "message": {
    "messageId": "…",
    "threadId": "…",
    "from": "Sinead <sineadpatience@me.com>",
    "to": "…",
    "subject": "…",
    "snippet": "…",
    "date": "…"
  }
}
```

A flat v1 shape (`from` / `subject` / `messageId` / `snippet` / `body` at the top level) is also accepted. `from` is required (`message.from` counts). Identity is `messageId`, else `idempotencyKey`, else a hash of from/subject/thread/snippet. Stored `source_key` is `webhook:<messageId>` so Gmail OAuth ingest of the same mailbox does not collide.

## Pipeline

1. Authenticate the hook secret.
2. Upsert into `email_inbound_messages` (same store as Gmail ingest).
3. Classify with deterministic rules, or pin via `ruleId`.
4. Set `ai_triage_status=skipped` so the AI pump never examines the message.
5. Emit a work item via the same `emit_for_inbound_message` path as rule-matched mail.

Responses: `202` created, `200` duplicate (same ids). `itemId` is null when the resolved category has no work-item policy. Both 2xx codes are success for Apps Script.

## Out of scope

Multi-rule UI, HMAC signing, IP allowlists, and native Gmail push. No production mailbox install from this change.
