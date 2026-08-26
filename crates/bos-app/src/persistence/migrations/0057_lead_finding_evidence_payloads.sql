UPDATE lead_findings
SET evidence_json = json_object(
    'evidence_id', 'lead_evidence_' || finding_id,
    'source', json_object(
        'source_id', json_extract(evidence_json, '$.source_id'),
        'kind', 'other',
        'display_name', json_extract(evidence_json, '$.source_display_name'),
        'url', json_extract(evidence_json, '$.source_url')
    ),
    'policy', json_object(
        'access_mode', 'approved_source_import',
        'broad_access_allowed', json('false'),
        'automated_outreach_allowed', json('false')
    ),
    'item_url', json_extract(evidence_json, '$.item_url'),
    'captured_at_ms', json_extract(evidence_json, '$.captured_at_ms'),
    'evidence_quote', json_extract(evidence_json, '$.evidence_quote'),
    'content_hash', NULL
)
WHERE json_valid(evidence_json)
  AND json_type(evidence_json, '$.evidence_id') IS NULL
  AND json_type(evidence_json, '$.source_display_name') IS NOT NULL
  AND json_type(evidence_json, '$.evidence_quote') IS NOT NULL;
