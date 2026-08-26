CREATE INDEX IF NOT EXISTS accounting_customer_snapshots_email
    ON accounting_customer_snapshots (client_id, lower(email));
