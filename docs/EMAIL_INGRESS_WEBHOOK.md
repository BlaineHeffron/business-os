# Email ingress webhook

Authenticated inbound HTTP webhook that opens a work item through the existing email triage → work queue path **without** an LLM classification call.

Gmail OAuth ingest is unchanged. This is the durable home for already-filtered events (external mail hooks, monitors, forms). Client scripts are not stored in this repository.

## Setup

1. Set `BOS_EMAIL_INGRESS_WEBHOOK_SECRET` on the BusinessOS host. Unset = the route 404s (fail closed).
2. `POST /api/webhooks/email-ingress`
3. Auth (either):
   - `Authorization: Bearer <secret>`
   - `X-Bos-Hook-Token: <secret>`
4. Operator tokens are rejected. Missing/wrong secret → 401.

Give the target triage category a work-queue policy with `create_work_item` and at least one packet kind. Without that policy, the message is stored and AI is skipped, but no work item is opened.

## curl

```bash
curl -sS -X POST "$BOS_URL/api/webhooks/email-ingress" \
  -H "Authorization: Bearer $BOS_EMAIL_INGRESS_WEBHOOK_SECRET" \
  -H "Content-Type: application/json" \
  -d '{
    "from": "Ada <ada@example.com>",
    "to": "ops@example.com",
    "subject": "Site change",
    "threadId": "thr-1",
    "messageId": "msg-1",
    "snippet": "The homepage copy changed.",
    "source": "email-hook"
  }'
```

## Payload

Flat v1 JSON:

```json
{
  "from": "Ada <ada@example.com>",
  "to": "optional",
  "subject": "…",
  "threadId": "optional",
  "messageId": "optional",
  "snippet": "…",
  "body": "optional plain text",
  "receivedAt": "ISO-8601 optional",
  "ruleId": "optional — pins that triage rule's category",
  "clientId": "optional — must match BOS_CLIENT_ID when set",
  "source": "email-hook",
  "idempotencyKey": "optional"
}
```

A nested `message` object is also accepted (`message.from`, `message.messageId`, `message.subject`, `message.snippet`, `message.date`, …). Extra fields are ignored. Optional top-level `ruleId` / `clientId` / `body` work with either shape.

`from` is required (`message.from` counts). Identity is `messageId`, else `idempotencyKey`, else a hash of from/subject/thread/snippet. Stored `source_key` is `webhook:<messageId>` so Gmail OAuth ingest of the same mailbox does not collide.

## Pipeline

1. Authenticate the hook secret.
2. Upsert into `email_inbound_messages` (same store as Gmail ingest).
3. Classify with deterministic rules, or pin via `ruleId`.
4. Set `ai_triage_status=skipped` so the AI pump never examines the message.
5. Emit a work item via the same `emit_for_inbound_message` path as rule-matched mail.

Responses: `202` created, `200` duplicate (same ids). `itemId` is null when the resolved category has no work-item policy. Both 2xx codes are success for idempotent clients.

## Out of scope

Client mailbox scripts, multi-rule UI, HMAC signing, IP allowlists, and native Gmail push. No production deploy from this change.
