ALTER TABLE email_inbound_messages
    ADD COLUMN sender_local_part TEXT;

ALTER TABLE email_inbound_messages
    ADD COLUMN sender_domain TEXT;

ALTER TABLE email_inbound_messages
    ADD COLUMN sender_automation_local_part INTEGER NOT NULL DEFAULT 0;

ALTER TABLE email_inbound_messages
    ADD COLUMN sender_header_identity_blocked INTEGER NOT NULL DEFAULT 0;

ALTER TABLE email_inbound_messages
    ADD COLUMN sender_identity_block_reason TEXT;

UPDATE email_inbound_messages
SET sender_local_part = lower(substr(sender_email, 1, instr(sender_email, '@') - 1)),
    sender_domain = lower(substr(sender_email, instr(sender_email, '@') + 1))
WHERE sender_email IS NOT NULL
  AND instr(sender_email, '@') > 1;

UPDATE email_inbound_messages
SET sender_automation_local_part = 1
WHERE sender_local_part IS NOT NULL
  AND (
    lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) IN (
      'automated', 'automation', 'auto', 'bounce', 'bounces',
      'do-not-reply', 'donotreply', 'email', 'mail', 'mailer',
      'no-reply', 'noreply', 'notification', 'notifications',
      'postmaster', 'receipt', 'receipts', 'robot', 'update', 'updates'
    )
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'automated[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'automation[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'auto[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'bounce[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'bounces[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'do-not-reply[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'donotreply[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'email[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'mail[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'mailer[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'no-reply[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'noreply[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'notification[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'notifications[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'postmaster[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'receipt[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'receipts[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'robot[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'update[+-]*'
    OR lower(replace(replace(sender_local_part, '_', '-'), '.', '-')) GLOB 'updates[+-]*'
  );

UPDATE email_inbound_messages
SET sender_header_identity_blocked = 1,
    sender_identity_block_reason = (
        SELECT CASE
            WHEN lower(COALESCE(json_extract(h.value, '$[0]'), '')) = 'auto-submitted'
                 AND lower(trim(COALESCE(json_extract(h.value, '$[1]'), ''))) NOT IN ('', 'no')
                THEN 'automated_email_headers'
            WHEN lower(COALESCE(json_extract(h.value, '$[0]'), ''))
                 IN ('list-id', 'list-owner', 'list-post', 'list-unsubscribe')
                THEN 'mailing_list_headers'
            WHEN lower(COALESCE(json_extract(h.value, '$[0]'), '')) = 'precedence'
                 AND lower(trim(COALESCE(json_extract(h.value, '$[1]'), '')))
                    IN ('bulk', 'junk', 'list')
                THEN 'bulk_email_headers'
        END
        FROM json_each(headers_json) h
        WHERE (lower(COALESCE(json_extract(h.value, '$[0]'), '')) = 'auto-submitted'
               AND lower(trim(COALESCE(json_extract(h.value, '$[1]'), ''))) NOT IN ('', 'no'))
           OR lower(COALESCE(json_extract(h.value, '$[0]'), ''))
              IN ('list-id', 'list-owner', 'list-post', 'list-unsubscribe')
           OR (lower(COALESCE(json_extract(h.value, '$[0]'), '')) = 'precedence'
               AND lower(trim(COALESCE(json_extract(h.value, '$[1]'), '')))
                  IN ('bulk', 'junk', 'list'))
        ORDER BY CAST(h.key AS INTEGER)
        LIMIT 1
    )
WHERE EXISTS (
    SELECT 1 FROM json_each(headers_json) h
    WHERE (lower(COALESCE(json_extract(h.value, '$[0]'), '')) = 'auto-submitted'
           AND lower(trim(COALESCE(json_extract(h.value, '$[1]'), ''))) NOT IN ('', 'no'))
       OR lower(COALESCE(json_extract(h.value, '$[0]'), ''))
          IN ('list-id', 'list-owner', 'list-post', 'list-unsubscribe')
       OR (lower(COALESCE(json_extract(h.value, '$[0]'), '')) = 'precedence'
           AND lower(trim(COALESCE(json_extract(h.value, '$[1]'), '')))
              IN ('bulk', 'junk', 'list'))
);

CREATE INDEX email_inbound_messages_sender_identity_idx
    ON email_inbound_messages (
        client_id,
        sender_domain,
        sender_automation_local_part,
        sender_header_identity_blocked
    );
