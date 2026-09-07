# Apps Script → BusinessOS email ingress

Posts matching Gmail into `POST /api/webhooks/email-ingress`. Secrets stay in Script Properties.

Companion to issue #14. For a generic routine-webhook client see issue #12.

## Install

1. [script.google.com](https://script.google.com) → New project. Paste `Code.gs`. Optional: copy `appsscript.json`.
2. Script properties:

| Property | Required | Example |
|----------|----------|---------|
| `WEBHOOK_URL` | yes | `https://bos.example/api/webhooks/email-ingress` |
| `WEBHOOK_SECRET` | yes | same value as `BOS_EMAIL_INGRESS_WEBHOOK_SECRET` |
| `GMAIL_QUERY` | yes | `from:sineadpatience@me.com newer_than:14d -label:bos-webhooked` |
| `PROCESSED_LABEL` | no | `bos-webhooked` |
| `RULE_ID` | no | BusinessOS triage rule id to pin |

3. Run `installTrigger` once (Gmail + external request scopes). Default poll: every 5 minutes.

The script POSTs the nested issue #12 payload (`source`, `version`, `receivedAt`, `query`, `message.messageId` / `from` / `subject` / …). Optional `RULE_ID` is sent as top-level `ruleId`.

Point `WEBHOOK_URL` at `https://<host>/api/webhooks/email-ingress`. The resolved email-triage category needs a work-queue policy or no work item is opened.

## Auth

`Authorization: Bearer <WEBHOOK_SECRET>` (same value as `BOS_EMAIL_INGRESS_WEBHOOK_SECRET`).

On HTTP 2xx the script labels the thread so it is not posted again.

Do not commit mailbox credentials or the webhook secret.

See [docs/EMAIL_INGRESS_WEBHOOK.md](../../docs/EMAIL_INGRESS_WEBHOOK.md).
