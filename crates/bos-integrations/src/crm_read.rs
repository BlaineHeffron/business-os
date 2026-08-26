//! Provider-neutral CRM snapshot read seam. BusinessOS uses these records to
//! build a local, offline-queryable CRM cache; callers pass provider
//! credentials explicitly and this crate never reads env.

pub const CRM_MAX_PAGE_SIZE: u32 = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrmReadError {
    RateLimited {
        retry_after_ms: Option<u64>,
        message: String,
    },
    Retryable {
        code: String,
        message: String,
    },
    Permanent {
        code: String,
        message: String,
    },
}

impl std::fmt::Display for CrmReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RateLimited { message, .. } => write!(formatter, "rate_limited: {message}"),
            Self::Retryable { code, message } | Self::Permanent { code, message } => {
                write!(formatter, "{code}: {message}")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrmPageRequest {
    pub cursor: Option<String>,
    pub page_size: u32,
}

impl CrmPageRequest {
    pub fn effective_page_size(&self) -> u32 {
        self.page_size.clamp(1, CRM_MAX_PAGE_SIZE)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrmPage<T> {
    pub records: Vec<T>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrmContactRecord {
    pub provider_contact_id: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub company: Option<String>,
    pub phone: Option<String>,
    pub lifecycle_stage: Option<String>,
    pub owner: Option<String>,
    pub last_activity_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrmDealRecord {
    pub provider_deal_id: String,
    pub name: Option<String>,
    pub stage: Option<String>,
    pub amount_cents: Option<i64>,
    pub currency: Option<String>,
    pub pipeline: Option<String>,
    pub close_date: Option<String>,
    pub associated_contact_ids: Vec<String>,
    pub associated_contact_email: Option<String>,
    pub associated_contact_company: Option<String>,
}

pub trait CrmReadClient {
    fn list_contacts_page(
        &self,
        request: &CrmPageRequest,
    ) -> Result<CrmPage<CrmContactRecord>, CrmReadError>;

    fn list_deals_page(
        &self,
        request: &CrmPageRequest,
    ) -> Result<CrmPage<CrmDealRecord>, CrmReadError>;
}
