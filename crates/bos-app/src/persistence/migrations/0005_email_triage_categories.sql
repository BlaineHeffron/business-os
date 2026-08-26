-- email_triage slice: operator-defined input categories. Categories are data:
-- rules pin them, the AI classifier reads the catalog (descriptions included)
-- as its schema, and future packet policy keys output suggestions off them.

CREATE TABLE email_triage_categories (
    client_id TEXT NOT NULL,
    category_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    color TEXT NOT NULL DEFAULT '#71717a',
    sort INTEGER NOT NULL DEFAULT 100,
    is_system INTEGER NOT NULL DEFAULT 0,
    deleted INTEGER NOT NULL DEFAULT 0,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (client_id, category_id)
);
