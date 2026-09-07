# Gmail webhook triggers

Reusable Gmail → outbound webhook for agent routines (Zapier-free).

The installable Apps Script lives in [`examples/gmail-webhook-trigger/`](../examples/gmail-webhook-trigger/).

## When to use

- Wake a Grok Bot / BusinessOS routine from specific mail (`from:`, label, subject patterns) without a long poll.
- First consumer: Sinead site-change mail → Vera routine webhook (prefer this over a 30‑minute Gmail poll).

## Contract

See the example README for Script Properties, JSON payload, idempotency label, and auth.

## Shepherd

Ship via PR → Dueno/autoReview → merge. Installing the script in a live mailbox is a **Blaine/Vera** deploy step — agents must not roll it out recklessly.
