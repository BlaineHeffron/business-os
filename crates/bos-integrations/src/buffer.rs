//! Buffer GraphQL write adapter for approval-gated social posts. Credentials
//! arrive only in [`BufferWriteConfig`]; the persisted outbox payload contains
//! the exact approved post snapshot and a channel-scoped idempotency key.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const DEFAULT_BUFFER_API_URL: &str = "https://api.buffer.com";
/// Buffer channel platform key that needs Instagram-specific post metadata.
/// Channel configuration must use exactly this value (it is lower-cased on load);
/// a different spelling silently skips the metadata and Buffer rejects the post.
pub const INSTAGRAM_PLATFORM: &str = "instagram";
/// Buffer's service key for Facebook channels.
pub const FACEBOOK_PLATFORM: &str = "facebook";
/// Buffer exposes Google Business Profile channels as `googlebusiness`, while
/// the corresponding create-post metadata field is named `google`.
pub const GOOGLE_BUSINESS_PLATFORM: &str = "googlebusiness";
pub const LINKEDIN_PLATFORM: &str = "linkedin";
pub const TWITTER_PLATFORM: &str = "twitter";
pub const SUPPORTED_PLATFORMS: [&str; 5] = [
    FACEBOOK_PLATFORM,
    GOOGLE_BUSINESS_PLATFORM,
    INSTAGRAM_PLATFORM,
    LINKEDIN_PLATFORM,
    TWITTER_PLATFORM,
];

pub fn supports_platform(raw: &str) -> bool {
    let platform = raw.trim().to_ascii_lowercase();
    SUPPORTED_PLATFORMS.contains(&platform.as_str())
}

#[derive(Debug, Clone)]
pub struct BufferWriteConfig {
    pub api_url: String,
    pub access_token: Option<String>,
    pub write_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BufferWriteError {
    Retryable {
        code: String,
        message: String,
        retry_after_secs: Option<u64>,
    },
    Permanent {
        code: String,
        message: String,
    },
    /// The create request may have reached Buffer, but no authoritative
    /// response proved whether the post was created.
    OutcomeUnknown {
        code: String,
        message: String,
    },
}

fn retryable(
    code: &str,
    message: impl Into<String>,
    retry_after_secs: Option<u64>,
) -> BufferWriteError {
    BufferWriteError::Retryable {
        code: code.to_string(),
        message: message.into(),
        retry_after_secs,
    }
}

fn permanent(code: &str, message: impl Into<String>) -> BufferWriteError {
    BufferWriteError::Permanent {
        code: code.to_string(),
        message: message.into(),
    }
}

fn outcome_unknown(code: &str, message: impl Into<String>) -> BufferWriteError {
    BufferWriteError::OutcomeUnknown {
        code: code.to_string(),
        message: message.into(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BufferScheduleMode {
    Queue,
    Scheduled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BufferApprovalMetadata {
    pub approved_by: String,
    pub approved_at_ms: u64,
    pub approved_revision: u64,
}

/// Outbox payload for `provider = "buffer", capability = "create_post"`.
/// One payload targets exactly one Buffer channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BufferPostOutboxPayload {
    pub schema_version: u32,
    pub client_id: String,
    pub proposal_id: String,
    pub target_id: String,
    pub channel_id: String,
    pub channel_name: String,
    pub platform: String,
    pub canonical_url: String,
    pub tracked_url: String,
    pub text: String,
    pub image_url: Option<String>,
    pub utm_json: String,
    pub schedule_mode: BufferScheduleMode,
    pub due_at: Option<String>,
    pub approval: BufferApprovalMetadata,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferPostResponse {
    pub executed: bool,
    pub dry_run: bool,
    pub post_id: Option<String>,
    pub status: Option<String>,
    pub due_at: Option<String>,
}

pub struct BufferHttpResponse {
    pub status: u16,
    pub retry_after_secs: Option<u64>,
    pub body: Value,
}

pub trait BufferHttp: Send + Sync {
    fn post_graphql(
        &self,
        api_url: &str,
        access_token: &str,
        idempotency_key: &str,
        body: &Value,
    ) -> Result<BufferHttpResponse, BufferWriteError>;
}

#[derive(Clone)]
pub struct ReqwestBufferHttpClient {
    client: reqwest::blocking::Client,
}

impl Default for ReqwestBufferHttpClient {
    fn default() -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self { client }
    }
}

impl BufferHttp for ReqwestBufferHttpClient {
    fn post_graphql(
        &self,
        api_url: &str,
        access_token: &str,
        idempotency_key: &str,
        body: &Value,
    ) -> Result<BufferHttpResponse, BufferWriteError> {
        let response = self
            .client
            .post(api_url)
            .bearer_auth(access_token)
            // Buffer's public schema has no client idempotency field. Keep the
            // stable channel key at the HTTP boundary and in the outbox state.
            .header("Idempotency-Key", idempotency_key)
            .json(body)
            .send()
            .map_err(|err| {
                if err.is_connect() {
                    retryable("buffer_connect_failed", err.to_string(), None)
                } else if err.is_builder() {
                    permanent("buffer_request_invalid", err.to_string())
                } else {
                    outcome_unknown("buffer_delivery_outcome_unknown", err.to_string())
                }
            })?;
        let status = response.status().as_u16();
        let retry_after_secs = response
            .headers()
            .get("Retry-After")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        let body = response.json::<Value>().map_err(|err| {
            if status == 429 {
                retryable("buffer_http_retryable", err.to_string(), retry_after_secs)
            } else if (200..300).contains(&status) || status >= 500 {
                outcome_unknown("buffer_delivery_outcome_unknown", err.to_string())
            } else {
                permanent("buffer_response_invalid", err.to_string())
            }
        })?;
        Ok(BufferHttpResponse {
            status,
            retry_after_secs,
            body,
        })
    }
}

pub trait BufferExecutionClient: Send + Sync {
    fn create_post(
        &self,
        payload: &BufferPostOutboxPayload,
    ) -> Result<BufferPostResponse, BufferWriteError>;
}

#[derive(Debug, Clone, Default)]
pub struct DryRunBufferClient;

impl BufferExecutionClient for DryRunBufferClient {
    fn create_post(
        &self,
        payload: &BufferPostOutboxPayload,
    ) -> Result<BufferPostResponse, BufferWriteError> {
        validate_payload(payload)?;
        Ok(BufferPostResponse {
            executed: false,
            dry_run: true,
            post_id: None,
            status: None,
            due_at: payload.due_at.clone(),
        })
    }
}

pub struct LiveBufferClient<C: BufferHttp = ReqwestBufferHttpClient> {
    http: Arc<C>,
    api_url: String,
    access_token: Arc<str>,
}

impl LiveBufferClient<ReqwestBufferHttpClient> {
    pub fn from_config(config: &BufferWriteConfig) -> Result<Self, BufferWriteError> {
        Self::new(Arc::new(ReqwestBufferHttpClient::default()), config)
    }
}

impl<C: BufferHttp> LiveBufferClient<C> {
    pub fn new(http: Arc<C>, config: &BufferWriteConfig) -> Result<Self, BufferWriteError> {
        let api_url = config.api_url.trim();
        if !valid_https_url(api_url) {
            return Err(permanent(
                "buffer_api_url_invalid",
                "Buffer API URL must be an https URL",
            ));
        }
        let access_token = config
            .access_token
            .as_deref()
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                permanent(
                    "buffer_access_token_missing",
                    "Buffer access token is required for live writes",
                )
            })?;
        Ok(Self {
            http,
            api_url: api_url.to_string(),
            access_token: Arc::from(access_token.to_string()),
        })
    }

    fn request_body(payload: &BufferPostOutboxPayload) -> Value {
        let mut input = serde_json::Map::from_iter([
            ("text".to_string(), json!(payload.text)),
            ("channelId".to_string(), json!(payload.channel_id)),
            ("schedulingType".to_string(), json!("automatic")),
            ("aiAssisted".to_string(), json!(true)),
            ("source".to_string(), json!("businessos")),
        ]);
        match payload.schedule_mode {
            BufferScheduleMode::Queue => {
                input.insert("mode".to_string(), json!("addToQueue"));
            }
            BufferScheduleMode::Scheduled => {
                input.insert("mode".to_string(), json!("customScheduled"));
                input.insert("dueAt".to_string(), json!(payload.due_at));
            }
        }
        if let Some(image_url) = payload.image_url.as_deref() {
            input.insert(
                "assets".to_string(),
                json!([{ "image": { "url": image_url } }]),
            );
        }
        // The social proposal contract represents ordinary feed/update posts.
        // Emit the provider metadata required by Buffer for those exact post
        // types. Facebook and LinkedIn need no metadata for this shape.
        // Google What's New field names follow Buffer's GooglePostMetadataInput
        // docs; live writes stay gated until an attended draft confirms them.
        let platform = payload.platform.trim().to_ascii_lowercase();
        match platform.as_str() {
            INSTAGRAM_PLATFORM => {
                // Verified live 2026-08-17: Instagram rejects createPost when
                // type and shouldShareToFeed are absent.
                input.insert(
                    "metadata".to_string(),
                    json!({ "instagram": { "type": "post", "shouldShareToFeed": true } }),
                );
            }
            GOOGLE_BUSINESS_PLATFORM => {
                input.insert(
                    "metadata".to_string(),
                    json!({
                        "google": {
                            "type": "whats_new",
                            "detailsWhatsNew": {
                                "button": "learn_more",
                                "link": payload.tracked_url,
                            }
                        }
                    }),
                );
            }
            _ => {}
        }
        json!({
            "query": "mutation CreateBusinessOsPost($input: CreatePostInput!) { createPost(input: $input) { ... on PostActionSuccess { post { id status dueAt } } ... on MutationError { message } } }",
            "variables": { "input": input },
        })
    }
}

impl<C: BufferHttp> BufferExecutionClient for LiveBufferClient<C> {
    fn create_post(
        &self,
        payload: &BufferPostOutboxPayload,
    ) -> Result<BufferPostResponse, BufferWriteError> {
        validate_payload(payload)?;
        let response = self.http.post_graphql(
            &self.api_url,
            self.access_token.as_ref(),
            &payload.idempotency_key,
            &Self::request_body(payload),
        )?;
        if response.status == 429 {
            return Err(retryable(
                "buffer_http_retryable",
                format!("Buffer HTTP {}", response.status),
                response.retry_after_secs,
            ));
        }
        if response.status >= 500 {
            return Err(outcome_unknown(
                "buffer_delivery_outcome_unknown",
                format!("Buffer HTTP {} after create submission", response.status),
            ));
        }
        if !(200..300).contains(&response.status) {
            return Err(permanent(
                "buffer_http_rejected",
                format!("Buffer HTTP {}", response.status),
            ));
        }
        if let Some(errors) = response.body.get("errors").and_then(Value::as_array) {
            if !errors.is_empty() {
                let code = errors
                    .first()
                    .and_then(|error| error.pointer("/extensions/code"))
                    .and_then(Value::as_str)
                    .unwrap_or("BUFFER_GRAPHQL_ERROR");
                return if code == "RATE_LIMIT_EXCEEDED" {
                    Err(retryable(
                        "buffer_graphql_retryable",
                        code,
                        response.retry_after_secs,
                    ))
                } else {
                    Err(outcome_unknown("buffer_delivery_outcome_unknown", code))
                };
            }
        }
        let result = &response.body["data"]["createPost"];
        if let Some(message) = result.get("message").and_then(Value::as_str) {
            return Err(permanent("buffer_post_rejected", message));
        }
        let post = result.get("post").ok_or_else(|| {
            permanent(
                "buffer_response_missing_post",
                "Buffer response did not include the created post",
            )
        })?;
        let post_id = post
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| {
                permanent(
                    "buffer_response_missing_post_id",
                    "Buffer response did not include a post id",
                )
            })?;
        Ok(BufferPostResponse {
            executed: true,
            dry_run: false,
            post_id: Some(post_id),
            status: post
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_string),
            due_at: post
                .get("dueAt")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }
}

pub fn buffer_execution_client(
    config: &BufferWriteConfig,
) -> Result<Box<dyn BufferExecutionClient>, BufferWriteError> {
    if config.write_enabled {
        Ok(Box::new(LiveBufferClient::from_config(config)?))
    } else {
        Ok(Box::new(DryRunBufferClient))
    }
}

pub fn validate_payload(payload: &BufferPostOutboxPayload) -> Result<(), BufferWriteError> {
    if payload.schema_version != 1
        || payload.client_id.trim().is_empty()
        || payload.proposal_id.trim().is_empty()
        || payload.target_id.trim().is_empty()
        || payload.channel_id.trim().is_empty()
        || payload.platform.trim().is_empty()
        || payload.text.trim().is_empty()
        || payload.idempotency_key.trim().is_empty()
        || payload.approval.approved_by.trim().is_empty()
        || payload.approval.approved_revision == 0
    {
        return Err(permanent(
            "buffer_payload_incomplete",
            "approved Buffer payload is incomplete",
        ));
    }
    if !supports_platform(&payload.platform) {
        return Err(permanent(
            "buffer_platform_unsupported",
            "Buffer platform is not supported by this approved post contract",
        ));
    }
    if !valid_https_url(&payload.canonical_url) || !valid_https_url(&payload.tracked_url) {
        return Err(permanent(
            "buffer_canonical_url_invalid",
            "canonical and tracked URLs must use https",
        ));
    }
    if !payload.text.contains(&payload.tracked_url) {
        return Err(permanent(
            "buffer_tracked_url_missing",
            "approved text must contain the tracked URL",
        ));
    }
    if let Some(image_url) = payload.image_url.as_deref() {
        if !valid_https_url(image_url) {
            return Err(permanent(
                "buffer_image_url_invalid",
                "Buffer image URL must use https",
            ));
        }
    } else if payload
        .platform
        .trim()
        .eq_ignore_ascii_case(INSTAGRAM_PLATFORM)
    {
        // Instagram feed posts always need media; fail at approval time instead
        // of burning an outbox attempt on a rejection from Buffer.
        return Err(permanent(
            "buffer_instagram_image_required",
            "Instagram posts require an image",
        ));
    }
    match payload.schedule_mode {
        BufferScheduleMode::Queue if payload.due_at.is_some() => Err(permanent(
            "buffer_queue_due_at_invalid",
            "queue mode cannot carry due_at",
        )),
        BufferScheduleMode::Scheduled
            if payload
                .due_at
                .as_deref()
                .is_none_or(|due_at| due_at.trim().is_empty()) =>
        {
            Err(permanent(
                "buffer_schedule_due_at_required",
                "scheduled mode requires due_at",
            ))
        }
        _ => Ok(()),
    }
}

fn valid_https_url(raw: &str) -> bool {
    url::Url::parse(raw.trim())
        .ok()
        .is_some_and(|url| url.scheme() == "https" && url.host_str().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeHttp {
        response: Mutex<Option<BufferHttpResponse>>,
        calls: Mutex<Vec<(String, String, Value)>>,
    }

    impl FakeHttp {
        fn respond(&self, status: u16, body: Value) {
            *self.response.lock().expect("lock") = Some(BufferHttpResponse {
                status,
                retry_after_secs: (status == 429).then_some(7),
                body,
            });
        }
    }

    impl BufferHttp for FakeHttp {
        fn post_graphql(
            &self,
            api_url: &str,
            _access_token: &str,
            idempotency_key: &str,
            body: &Value,
        ) -> Result<BufferHttpResponse, BufferWriteError> {
            self.calls.lock().expect("lock").push((
                api_url.to_string(),
                idempotency_key.to_string(),
                body.clone(),
            ));
            self.response
                .lock()
                .expect("lock")
                .take()
                .ok_or_else(|| permanent("fake_missing_response", "missing fake response"))
        }
    }

    fn payload() -> BufferPostOutboxPayload {
        BufferPostOutboxPayload {
            schema_version: 1,
            client_id: "client".to_string(),
            proposal_id: "social_1".to_string(),
            target_id: "target_1".to_string(),
            channel_id: "channel_1".to_string(),
            channel_name: "LinkedIn".to_string(),
            platform: "linkedin".to_string(),
            canonical_url: "https://example.com/post".to_string(),
            tracked_url: "https://example.com/post?utm_source=linkedin".to_string(),
            text: "Read it: https://example.com/post?utm_source=linkedin".to_string(),
            image_url: Some("https://example.com/image.jpg".to_string()),
            utm_json: r#"{"source":"linkedin"}"#.to_string(),
            schedule_mode: BufferScheduleMode::Scheduled,
            due_at: Some("2026-08-20T14:00:00Z".to_string()),
            approval: BufferApprovalMetadata {
                approved_by: "user_example".to_string(),
                approved_at_ms: 1_000,
                approved_revision: 2,
            },
            idempotency_key: "social:social_1:2:channel_1".to_string(),
        }
    }

    #[test]
    fn dry_run_validates_without_a_provider_call() {
        let result = DryRunBufferClient.create_post(&payload()).expect("dry run");
        assert!(result.dry_run);
        assert!(!result.executed);
        assert_eq!(result.post_id, None);
    }

    #[test]
    fn live_create_posts_exact_snapshot_with_channel_idempotency() {
        let http = Arc::new(FakeHttp::default());
        http.respond(
            200,
            json!({
                "data": { "createPost": { "post": {
                    "id": "buffer_post_1",
                    "status": "buffer",
                    "dueAt": "2026-08-20T14:00:00Z"
                } } }
            }),
        );
        let client = LiveBufferClient::new(
            Arc::clone(&http),
            &BufferWriteConfig {
                api_url: DEFAULT_BUFFER_API_URL.to_string(),
                access_token: Some("secret-never-persisted".to_string()),
                write_enabled: true,
            },
        )
        .expect("client");
        let result = client.create_post(&payload()).expect("create");
        assert_eq!(result.post_id.as_deref(), Some("buffer_post_1"));
        let calls = http.calls.lock().expect("lock");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, "social:social_1:2:channel_1");
        let input = &calls[0].2["variables"]["input"];
        assert_eq!(input["channelId"], "channel_1");
        assert_eq!(input["mode"], "customScheduled");
        assert_eq!(
            input["assets"][0]["image"]["url"],
            "https://example.com/image.jpg"
        );
        assert!(input.get("metadata").is_none());
        assert!(!calls[0].2.to_string().contains("secret-never-persisted"));
    }

    #[test]
    fn instagram_payloads_carry_required_post_type_metadata() {
        let http = Arc::new(FakeHttp::default());
        http.respond(
            200,
            json!({
                "data": { "createPost": { "post": {
                    "id": "buffer_post_ig",
                    "status": "draft",
                    "dueAt": null
                } } }
            }),
        );
        let client = LiveBufferClient::new(
            Arc::clone(&http),
            &BufferWriteConfig {
                api_url: DEFAULT_BUFFER_API_URL.to_string(),
                access_token: Some("secret".to_string()),
                write_enabled: true,
            },
        )
        .expect("client");
        let mut instagram = payload();
        instagram.platform = "instagram".to_string();
        instagram.channel_name = "Instagram".to_string();
        client.create_post(&instagram).expect("create");
        let calls = http.calls.lock().expect("lock");
        let input = &calls[0].2["variables"]["input"];
        assert_eq!(input["metadata"]["instagram"]["type"], "post");
        assert_eq!(input["metadata"]["instagram"]["shouldShareToFeed"], true);
    }

    #[test]
    fn connected_platforms_emit_only_required_provider_metadata() {
        let http = Arc::new(FakeHttp::default());
        let client = LiveBufferClient::new(
            Arc::clone(&http),
            &BufferWriteConfig {
                api_url: DEFAULT_BUFFER_API_URL.to_string(),
                access_token: Some("secret".to_string()),
                write_enabled: true,
            },
        )
        .expect("client");

        http.respond(
            200,
            json!({ "data": { "createPost": { "post": { "id": "fb_1" } } } }),
        );
        let mut facebook = payload();
        facebook.platform = FACEBOOK_PLATFORM.to_string();
        client.create_post(&facebook).expect("facebook create");

        http.respond(
            200,
            json!({ "data": { "createPost": { "post": { "id": "li_1" } } } }),
        );
        let mut linkedin = payload();
        linkedin.platform = LINKEDIN_PLATFORM.to_string();
        client.create_post(&linkedin).expect("linkedin create");

        http.respond(
            200,
            json!({ "data": { "createPost": { "post": { "id": "gbp_1" } } } }),
        );
        let mut google_business = payload();
        google_business.platform = GOOGLE_BUSINESS_PLATFORM.to_string();
        client
            .create_post(&google_business)
            .expect("google business create");

        let calls = http.calls.lock().expect("lock");
        assert!(calls[0].2["variables"]["input"].get("metadata").is_none());
        assert!(calls[1].2["variables"]["input"].get("metadata").is_none());
        let google = &calls[2].2["variables"]["input"]["metadata"]["google"];
        assert_eq!(google["type"], "whats_new");
        assert_eq!(google["detailsWhatsNew"]["button"], "learn_more");
        assert_eq!(
            google["detailsWhatsNew"]["link"],
            google_business.tracked_url
        );
    }

    #[test]
    fn platform_keys_are_accepted_case_insensitively() {
        assert!(supports_platform(" Facebook "));
        assert!(supports_platform("GoogleBusiness"));
        let mut mixed = payload();
        mixed.platform = "Instagram".to_string();
        validate_payload(&mixed).expect("mixed-case instagram");
    }

    #[test]
    fn unsupported_platforms_fail_closed_before_delivery() {
        for platform in ["pinterest", "tiktok"] {
            let mut unsupported = payload();
            unsupported.platform = platform.to_string();
            let err = validate_payload(&unsupported).expect_err("unsupported platform");
            assert!(matches!(
                err,
                BufferWriteError::Permanent { ref code, .. }
                    if code == "buffer_platform_unsupported"
            ));
        }
    }

    #[test]
    fn instagram_payloads_require_an_image() {
        let mut instagram = payload();
        instagram.platform = INSTAGRAM_PLATFORM.to_string();
        instagram.image_url = None;
        let err = validate_payload(&instagram).expect_err("image required");
        assert!(matches!(
            err,
            BufferWriteError::Permanent { ref code, .. } if code == "buffer_instagram_image_required"
        ));
        // Other platforms may still post without an image.
        let mut linkedin = payload();
        linkedin.image_url = None;
        validate_payload(&linkedin).expect("image optional elsewhere");
    }

    #[test]
    fn retries_only_known_rejections_and_marks_ambiguous_create_results_unknown() {
        let http = Arc::new(FakeHttp::default());
        let client = LiveBufferClient::new(
            Arc::clone(&http),
            &BufferWriteConfig {
                api_url: DEFAULT_BUFFER_API_URL.to_string(),
                access_token: Some("secret".to_string()),
                write_enabled: true,
            },
        )
        .expect("client");
        http.respond(429, json!({ "errors": [{ "message": "slow down" }] }));
        assert!(matches!(
            client.create_post(&payload()),
            Err(BufferWriteError::Retryable {
                retry_after_secs: Some(7),
                ..
            })
        ));
        http.respond(503, json!({ "errors": [{ "message": "unavailable" }] }));
        assert!(matches!(
            client.create_post(&payload()),
            Err(BufferWriteError::OutcomeUnknown { code, .. })
                if code == "buffer_delivery_outcome_unknown"
        ));
        http.respond(
            200,
            json!({ "errors": [{ "extensions": { "code": "TIMEOUT" } }] }),
        );
        assert!(matches!(
            client.create_post(&payload()),
            Err(BufferWriteError::OutcomeUnknown { code, .. })
                if code == "buffer_delivery_outcome_unknown"
        ));
        http.respond(
            200,
            json!({ "data": { "createPost": { "message": "channel rejected post" } } }),
        );
        assert!(matches!(
            client.create_post(&payload()),
            Err(BufferWriteError::Permanent { code, .. }) if code == "buffer_post_rejected"
        ));
    }
}
