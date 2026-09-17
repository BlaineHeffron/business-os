-- Published-article hero/og:image carried through ingest onto drafted targets.
ALTER TABLE social_published_sources
    ADD COLUMN image_url TEXT;
