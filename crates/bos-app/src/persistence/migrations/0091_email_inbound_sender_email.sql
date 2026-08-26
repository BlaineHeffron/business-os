ALTER TABLE email_inbound_messages
    ADD COLUMN sender_email TEXT;

UPDATE email_inbound_messages
SET sender_email = lower(trim(CASE
    WHEN instr(COALESCE(from_addr, ''), ',') > 0
    THEN substr(COALESCE(from_addr, ''), 1, instr(COALESCE(from_addr, ''), ',') - 1)
    ELSE COALESCE(from_addr, '')
END));

UPDATE email_inbound_messages
SET sender_email = lower(trim(substr(sender_email, instr(sender_email, '<') + 1,
    instr(sender_email, '>') - instr(sender_email, '<') - 1)))
WHERE instr(sender_email, '<') > 0
  AND instr(sender_email, '>') > instr(sender_email, '<');

UPDATE email_inbound_messages
SET sender_email = trim(sender_email, '"'' ')
WHERE sender_email IS NOT NULL;

UPDATE email_inbound_messages
SET sender_email = NULL
WHERE sender_email IS NOT NULL
  AND (sender_email = ''
       OR instr(sender_email, '@') = 0
       OR instr(substr(sender_email, instr(sender_email, '@') + 1), '.') = 0
       OR instr(substr(sender_email, instr(sender_email, '@') + 1), ' ') > 0);

CREATE INDEX email_inbound_messages_sender_email_idx
    ON email_inbound_messages (client_id, sender_email);
