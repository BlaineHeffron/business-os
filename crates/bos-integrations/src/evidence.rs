//! Inert evidence store for bounded agentic research.
//!
//! PR2 only constructs this from tests. The live research loop lands later; this
//! module is pure data assembly with no network or IO.

use std::collections::BTreeMap;
use std::sync::Arc;

use sha2::{Digest, Sha256 as Sha256Hasher};
use url::Url;

use crate::web_page_read::{normalize_page_text, registrable_domain, NORMALIZER_VERSION};

pub type EvidenceId = String;
pub type Sha256 = String;
type ContentSourceKey = (Sha256, String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidencePage {
    pub evidence_id: EvidenceId,
    pub requested_url: Url,
    pub final_url: Url,
    pub registrable_domain: String,
    pub fetched_at_ms: u64,
    pub http_status: u16,
    pub content_sha256: Sha256,
    pub normalized_text_sha256: Sha256,
    pub normalizer_version: u16,
    /// Exact display text shown to the model and used as grounding coordinate
    /// space. Canonical matching must slice this first, then canonicalize.
    pub normalized_text: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceSpan {
    pub evidence_id: EvidenceId,
    pub display_byte_start: u32,
    pub display_byte_end: u32,
    pub canonical_sha256: Sha256,
}

#[derive(Debug, Default)]
pub struct EvidenceStore {
    pages: BTreeMap<EvidenceId, EvidencePage>,
    content_index: BTreeMap<ContentSourceKey, EvidenceId>,
    next_id: u32,
}

impl EvidenceStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_html_page(
        &mut self,
        requested_url: Url,
        final_url: Url,
        fetched_at_ms: u64,
        http_status: u16,
        html: &str,
        max_chars: usize,
    ) -> EvidenceId {
        let content_sha256 = sha256_hex(html.as_bytes());
        let source_key = (content_sha256.clone(), final_url.as_str().to_string());
        if let Some(existing) = self.content_index.get(&source_key) {
            return existing.clone();
        }

        let normalized = normalize_page_text(html, Some(&final_url), max_chars);
        let normalized_text = normalized.layout_text;
        let evidence_id = format!("ev_{}", self.next_id);
        self.next_id += 1;
        let page = EvidencePage {
            evidence_id: evidence_id.clone(),
            requested_url,
            registrable_domain: final_url
                .host_str()
                .map(registrable_domain)
                .unwrap_or_default(),
            final_url,
            fetched_at_ms,
            http_status,
            content_sha256: content_sha256.clone(),
            normalized_text_sha256: sha256_hex(normalized_text.as_bytes()),
            normalizer_version: NORMALIZER_VERSION,
            normalized_text,
        };
        self.pages.insert(evidence_id.clone(), page);
        self.content_index.insert(source_key, evidence_id.clone());
        evidence_id
    }

    pub fn insert_html_page_urls(
        &mut self,
        requested_url: &str,
        final_url: &str,
        fetched_at_ms: u64,
        http_status: u16,
        html: &str,
        max_chars: usize,
    ) -> Result<EvidenceId, String> {
        let requested_url = Url::parse(requested_url)
            .map_err(|err| format!("unparseable requested_url {requested_url}: {err}"))?;
        let final_url = Url::parse(final_url)
            .map_err(|err| format!("unparseable final_url {final_url}: {err}"))?;
        Ok(self.insert_html_page(
            requested_url,
            final_url,
            fetched_at_ms,
            http_status,
            html,
            max_chars,
        ))
    }

    pub fn get(&self, evidence_id: &str) -> Option<&EvidencePage> {
        self.pages.get(evidence_id)
    }

    pub fn pages(&self) -> impl Iterator<Item = &EvidencePage> {
        self.pages.values()
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256Hasher::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web_page_read::normalize_page_text;

    #[test]
    fn insert_html_page_stores_layout_text_and_hashes() {
        let mut store = EvidenceStore::new();
        let requested = Url::parse("https://www.example.com/contact").unwrap();
        let final_url = Url::parse("https://example.com/contact").unwrap();
        let html = "<html><body><h1>Example Co</h1><p>Contact us</p></body></html>";
        let id = store.insert_html_page(
            requested.clone(),
            final_url.clone(),
            1_234,
            200,
            html,
            1_000,
        );

        assert_eq!(id, "ev_0");
        let page = store.get(&id).expect("stored page");
        let normalized = normalize_page_text(html, Some(&final_url), 1_000);
        assert_eq!(page.requested_url, requested);
        assert_eq!(page.final_url, final_url);
        assert_eq!(page.registrable_domain, "example.com");
        assert_eq!(page.normalizer_version, NORMALIZER_VERSION);
        assert_eq!(page.normalized_text, normalized.layout_text);
        assert_eq!(
            page.normalized_text_sha256,
            sha256_hex(normalized.layout_text.as_bytes())
        );
        assert_eq!(page.content_sha256, sha256_hex(html.as_bytes()));
    }

    #[test]
    fn insert_html_page_dedupes_identical_content_by_source_url_and_content_hash() {
        let mut store = EvidenceStore::new();
        let first = store.insert_html_page(
            Url::parse("https://example.com/a").unwrap(),
            Url::parse("https://example.com/a").unwrap(),
            1,
            200,
            "<html><body>Same</body></html>",
            1_000,
        );
        let second = store.insert_html_page(
            Url::parse("https://example.com/a?utm=1").unwrap(),
            Url::parse("https://example.com/a").unwrap(),
            2,
            200,
            "<html><body>Same</body></html>",
            1_000,
        );
        let third = store.insert_html_page(
            Url::parse("https://other.example.net/a").unwrap(),
            Url::parse("https://other.example.net/a").unwrap(),
            3,
            200,
            "<html><body>Same</body></html>",
            1_000,
        );
        let fourth = store.insert_html_page(
            Url::parse("https://example.com/c").unwrap(),
            Url::parse("https://example.com/c").unwrap(),
            4,
            200,
            "<html><body>Different</body></html>",
            1_000,
        );

        assert_eq!(first, "ev_0");
        assert_eq!(second, first);
        assert_eq!(third, "ev_1");
        assert_eq!(
            store.get(&third).expect("third page").registrable_domain,
            "example.net"
        );
        assert_eq!(fourth, "ev_2");
        assert_eq!(store.len(), 3);
    }
}
