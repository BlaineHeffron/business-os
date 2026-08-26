---
title: "Social publishing"
keywords: ["social", "Buffer", "channels", "approval", "schedule"]
order: 7
---

# Social publishing

## What this does

Use Social for standalone post-publication campaigns. For a planned blog campaign, use the Content campaign workspace so social posts wait for the blog adapter's validated canonical URL.

## Common tasks

- Select a published article or paste its canonical HTTPS URL.
- For a known published article, choose Generate with AI. Generated claims must be supported by the published source before a proposal is ready for review.
- Tailor the text, public image, tracking values, and timing for each configured channel.
- Save edits before deciding.
- Approve the exact text shown to queue every channel independently.
- Retry only a failed channel without touching successful channels.

## Notes

Published-content integrations provide article identity and metadata only; they cannot provide social copy or approve posts. Administrators can configure a private local AI model when needed. Live Buffer delivery is off until an administrator enables it. In validation mode, an approval checks the full workflow without creating a Buffer post. If someone else changes the proposal first, reload and review their changes before approving.

Pre-publication variants created in Content are intentionally hidden here and cannot be approved through this standalone surface. Approve them from the Content campaign workspace so they still wait for the blog's verified URL.
