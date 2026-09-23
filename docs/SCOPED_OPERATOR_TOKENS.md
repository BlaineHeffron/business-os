# Scoped operator tokens

The shared `BOS_OPERATOR_TOKEN` and personal `operator_users` tokens remain
**unscoped**: they may call every operator route.

Machine callers that only need a sliver of that surface — the Slack approval
bridge and a publishing agent on `/api/agent-mcp` — should hold a **scoped API
token** instead. A compromise of those processes then cannot reach accounting,
CRM, Gmail, admin settings, or user administration.

## Capabilities

| Capability | Grants |
| --- | --- |
| `social_publishing:read` | `GET /api/social-publishing/proposals` |
| `social_publishing:update` | `POST /api/social-publishing/proposals/{id}/update` |
| `social_publishing:approve` | `POST /api/social-publishing/proposals/{id}/action` (approve, reject, and redraft; redraft spawns the same typed-LLM draft as the generate route) |
| `social_publishing:stage` | `POST /api/social-publishing/proposals` (operator-authored copy) |
| `social_publishing:generate` | generate / preview kickoff routes |
| `agent_mcp:ingest` | MCP endpoint, `bos_social_published_content_ingest` and `bos_social_adhoc_source_create` only |

Unknown capability strings are rejected at mint time. An empty list is
rejected. There is no wildcard; unscoped access is only the existing env and
personal-user tokens.

Suggested grants:

- Slack bridge: `social_publishing:read`, `social_publishing:update`,
  `social_publishing:approve`, `agent_mcp:ingest` (sitemap ingest).
- Publishing agent: `agent_mcp:ingest` only.

## Minting

Unscoped operators (`require_all_scope`) mint tokens:

```bash
curl -sS -X POST "$BOS_URL/api/operator-tokens" \
  -H "Authorization: Bearer $BOS_OPERATOR_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "label": "Slack bridge",
    "capabilities": [
      "social_publishing:read",
      "social_publishing:update",
      "social_publishing:approve",
      "agent_mcp:ingest"
    ],
    "idempotency_key": "mint-slack-bridge"
  }'
```

The `token` field is the bearer secret and is returned **once**. Put that value
in the bridge or agent `BOS_OPERATOR_TOKEN` (or equivalent) environment; leave
the server's unscoped `BOS_OPERATOR_TOKEN` for humans and the SPA.

`GET /api/me` on a scoped token includes `capabilities` and uses the token id
as `actor_id` (receipts cannot spoof a human operator). Unscoped whoami omits
the capabilities field.

Disable, revoke, and rotate live at `/api/operator-tokens/{token_id}/action`
and `/api/operator-tokens/{token_id}/rotate-token`.

## Fail-closed defaults

`require_operator` / `authenticate` / `authenticate_operator` still mean
**unscoped**. Routes that accept a scoped token call `require_capability`
(or the MCP gate). Forgetting to opt a new route in keeps it unreachable to
the bridge and agent.

Scoped tokens cannot open a browser session, cannot mint further tokens, and
cannot use OAuth query-token connect URLs.
