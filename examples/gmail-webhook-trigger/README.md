# Gmail → webhook email trigger

Google Apps Script template that watches a Gmail search query and `POST`s matching messages to a webhook URL (Zapier-free).

Use this for BusinessOS / agent **routine webhooks** (e.g. wake Vera when Sinead’s site-change mail arrives) and any other client mail rule.

## Install

1. In the target Google account: [script.google.com](https://script.google.com) → New project.
2. Paste `Code.gs` into the default file; set the manifest from `appsscript.json` (Project Settings → Show `appsscript.json`).
3. **Project Settings → Script properties** (do not commit secrets):

| Property | Required | Example |
|----------|----------|---------|
| `WEBHOOK_URL` | yes | `https://…/hooks/…` |
| `GMAIL_QUERY` | yes | `from:sineadpatience@me.com newer_than:7d -label:bos-webhooked` |
| `WEBHOOK_SECRET` | no | shared bearer/HMAC secret |
| `WEBHOOK_AUTH_HEADER` | no | `Authorization` (default) |
| `PROCESSED_LABEL` | no | `bos-webhooked` (applied after success) |
| `MAX_THREADS` | no | `20` |

4. Run `installTrigger` once (authorize Gmail + external request scopes).
5. Default trigger: every **5 minutes** (`ScriptApp.ClockTriggerBuilder.everyMinutes(5)`). Adjust in `installTrigger`.

## Payload (JSON POST)

```json
{
  "source": "business-os.gmail-webhook-trigger",
  "version": 1,
  "receivedAt": "2026-09-07T15:00:00.000Z",
  "query": "from:sineadpatience@me.com …",
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

Full bodies and attachments are **not** included by default.

## Auth

If `WEBHOOK_SECRET` is set, the script sends:

`Authorization: Bearer <WEBHOOK_SECRET>`

(or the header named in `WEBHOOK_AUTH_HEADER`).

## Idempotency

On HTTP 2xx the script applies `PROCESSED_LABEL` (default `bos-webhooked`). Include `-label:bos-webhooked` in `GMAIL_QUERY` so the same message is not posted twice.

## Example: Sinead site-change → Vera routine

Suggested query (tune to the real subject/from pattern):

```text
from:sineadpatience@me.com newer_than:14d -label:bos-webhooked
```

Point `WEBHOOK_URL` at the Vera routine webhook from the Grok Bot routine panel. Vera owns converting the poll-based watch to this webhook.

## Security

- Never commit mailbox credentials or webhook secrets into `business-os`.
- Prefer a dedicated filter label and least-privilege Apps Script project.
- Rotate `WEBHOOK_SECRET` if the URL is shared or leaked.

See also issue tracking this feature in the repo.
