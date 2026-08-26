//! Social publishing domain logic: normalize a canonical published URL and
//! exact per-channel proposals, then deliver immutable approval snapshots via
//! Buffer. No LLM or agent path receives Buffer credentials.

use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
use std::collections::VecDeque;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};

use bos_contracts::content_drafts::ContentDraftStatus;
use bos_contracts::receipt::ActorKindDto;
use bos_contracts::social_publishing::{
    SocialDraftPreviewGenerateRequest, SocialPostProposal, SocialProposalStageRequest,
    SocialProposalStatus, SocialProposalTarget, SocialProposalTargetInput,
    SocialProposalUpdateRequest, SocialPublishedContentIngressRequest, SocialPublishedSource,
    SocialPublishingChannel, SocialScheduleMode, SocialSourceGenerationStatus, SocialUtmParameters,
};
use bos_integrations::buffer::{
    self, BufferApprovalMetadata, BufferPostOutboxPayload, BufferScheduleMode, BufferWriteConfig,
    BufferWriteError,
};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::store;
use crate::env_registry;
use crate::outbox::{retry_backoff_ms, AttemptOutcome, ClaimedJob, NewOutboxJob};
use crate::slices::async_kickoff::{
    KickoffCapacity, KickoffDecision, KickoffSpec, RecordedKickoff,
};
use crate::slices::mutation_context::MutationContext;
use crate::store_core::{MutationOutcome, StoreError};

use bos_integrations::llm_typed_tasks::{
    TypedLlmAuthority, TypedLlmExecutionPolicy, TypedLlmExecutionRoute, TypedLlmFallbackPolicy,
    TypedLlmProviderPolicy, TypedLlmRedactionPolicy, TypedLlmResponseFormat, TypedLlmRetryPolicy,
    TypedLlmSafetyPolicy, TypedLlmSourceEntity, TypedLlmTaskCapabilities, TypedLlmTaskClass,
    TypedLlmTaskInput, TypedLlmTaskOutputEnvelope, TypedLlmTaskRequest, TypedLlmTaskSpec,
    TypedLlmTextBlock,
};

pub const PROVIDER_BUFFER: &str = "buffer";
pub const CAPABILITY_CREATE_POST: &str = "create_post";
pub const DRAFT_PURPOSE: &str = "social_post_draft";
pub const DRAFT_SCHEMA_REF: &str = "bos.social_publishing.campaign_draft.v1";
pub const PREVIEW_SOURCE_KIND: &str = "businessos_content_preview";
const GENERATOR_ACTOR: &str = "social_draft_generator";
const MAX_POST_TEXT_CHARS: usize = 10_000;
const MAX_UTM_VALUE_CHARS: usize = 200;
const MAX_SOURCE_TITLE_CHARS: usize = 300;
const MAX_SOURCE_EXCERPT_CHARS: usize = 8_000;
const MAX_SOURCE_BODY_CHARS: usize = 40_000;
const MAX_GENERATION_ATTEMPTS: u64 = 2;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SocialDraftOutput {
    targets: Vec<SocialDraftTargetOutput>,
    confidence: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SocialDraftTargetOutput {
    target_ref: String,
    text: String,
    utm_source: String,
    utm_medium: String,
    utm_campaign: String,
    #[serde(default)]
    utm_content: Option<String>,
    source_quotes: Vec<String>,
}

pub fn configured_channels() -> Result<Vec<SocialPublishingChannel>, StoreError> {
    let raw = env_registry::string(&env_registry::BOS_BUFFER_CHANNELS_JSON)
        .ok_or_else(|| StoreError::Domain("social_channels_not_configured".to_string()))?;
    let channels: Vec<SocialPublishingChannel> = serde_json::from_str(&raw)
        .map_err(|_| StoreError::Domain("social_channels_config_invalid".to_string()))?;
    if channels.is_empty() {
        return Err(StoreError::Domain(
            "social_channels_not_configured".to_string(),
        ));
    }
    let mut ids = BTreeSet::new();
    for channel in &channels {
        if channel.channel_id.trim().is_empty()
            || channel.name.trim().is_empty()
            || !valid_platform(&channel.platform)
            || !ids.insert(channel.channel_id.trim().to_string())
        {
            return Err(StoreError::Domain(
                "social_channels_config_invalid".to_string(),
            ));
        }
    }
    Ok(channels
        .into_iter()
        .map(|channel| SocialPublishingChannel {
            channel_id: channel.channel_id.trim().to_string(),
            name: channel.name.trim().to_string(),
            platform: channel.platform.trim().to_ascii_lowercase(),
        })
        .collect())
}

pub fn buffer_live_enabled(conn: &Connection, client_id: &str) -> bool {
    crate::slices::admin_settings::service::flag(
        conn,
        client_id,
        &env_registry::BOS_BUFFER_WRITE_ENABLED,
    )
    .unwrap_or(false)
}

pub fn published_sources(
    conn: &Connection,
    client_id: &str,
) -> Result<Vec<SocialPublishedSource>, StoreError> {
    let mut sources = store::list_sources(conn, client_id, 100)?;
    sources.retain(|source| source.source_kind != PREVIEW_SOURCE_KIND);
    let stored_draft_ids = sources
        .iter()
        .filter_map(|source| source.source_content_draft_id.clone())
        .collect::<BTreeSet<_>>();
    let proposals = store::list_proposals(conn, client_id, 200)?;
    let drafts = crate::slices::content_drafts::store::list_drafts(conn, client_id, None, 100)?;
    for entry in drafts {
        if entry.draft.status != ContentDraftStatus::Approved
            || stored_draft_ids.contains(&entry.draft.draft_id)
        {
            continue;
        }
        let Some(job) = entry.outbox_job else {
            continue;
        };
        let Some(canonical_url) = job.provider_object_id else {
            continue;
        };
        if job.status != crate::outbox::STATUS_DELIVERED || job.dry_run == Some(true) {
            continue;
        }
        let source_id = source_id_for(client_id, "businessos_content", &entry.draft.draft_id);
        let proposal_id = proposals
            .iter()
            .find(|proposal| proposal.proposal.source_id.as_deref() == Some(&source_id))
            .map(|proposal| proposal.proposal.proposal_id.clone());
        sources.push(SocialPublishedSource {
            source_id,
            source_kind: "businessos_content".to_string(),
            external_id: entry.draft.draft_id.clone(),
            source_content_draft_id: Some(entry.draft.draft_id),
            source_content_draft_revision: Some(entry.revision),
            title: entry.draft.title,
            canonical_url,
            excerpt: None,
            published_at: None,
            generation_status: if proposal_id.is_some() {
                SocialSourceGenerationStatus::ProposalStaged
            } else {
                SocialSourceGenerationStatus::Ready
            },
            generation_run_id: None,
            generation_error: None,
            proposal_id,
            revision: 0,
        });
    }
    sources.sort_by(|left, right| left.title.cmp(&right.title));
    Ok(sources)
}

pub fn stage_request(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    actor_kind: ActorKindDto,
    request: &SocialProposalStageRequest,
    now_ms: u64,
) -> Result<(MutationOutcome, String), StoreError> {
    require_idempotency_key(&request.idempotency_key)?;
    let channels = configured_channels()?;
    let proposal_id = proposal_id_for(client_id, &request.idempotency_key);
    let canonical_url = normalize_canonical_url(&request.canonical_url)?;
    validate_published_source(
        conn,
        client_id,
        request.source_id.as_deref(),
        request.source_content_draft_id.as_deref(),
        request.source_content_draft_revision,
        &canonical_url,
    )?;
    validate_registered_source(
        conn,
        client_id,
        request.source_id.as_deref(),
        request.source_content_draft_id.as_deref(),
        request.source_content_draft_revision,
        &canonical_url,
    )?;
    let targets = normalize_targets(&proposal_id, &canonical_url, &channels, &request.targets)?;
    let proposal = SocialPostProposal {
        proposal_id: proposal_id.clone(),
        source_id: clean_optional(request.source_id.as_deref()),
        source_content_draft_id: clean_optional(request.source_content_draft_id.as_deref()),
        source_content_draft_revision: request.source_content_draft_revision,
        canonical_url,
        status: SocialProposalStatus::Staged,
        targets,
        approved_by: None,
        approved_revision: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    };
    let outcome = store::stage_proposal(
        conn,
        client_id,
        actor_id,
        actor_kind,
        &proposal,
        &request.idempotency_key,
    )?;
    Ok((outcome, proposal_id))
}

pub fn update_request(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    proposal_id: &str,
    request: &SocialProposalUpdateRequest,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    require_idempotency_key(&request.idempotency_key)?;
    let current = store::get_proposal(conn, client_id, proposal_id)?
        .ok_or_else(|| StoreError::Domain("social_proposal_not_found".to_string()))?;
    if crate::slices::content_plans::store::social_proposal_campaign_locked(
        conn,
        client_id,
        proposal_id,
    )? {
        return Err(StoreError::Domain(
            "social_proposal_campaign_locked".to_string(),
        ));
    }
    let canonical_url = normalize_canonical_url(&request.canonical_url)?;
    validate_published_source(
        conn,
        client_id,
        current.proposal.source_id.as_deref(),
        current.proposal.source_content_draft_id.as_deref(),
        current.proposal.source_content_draft_revision,
        &canonical_url,
    )?;
    let channels = configured_channels()?;
    let targets = normalize_targets(proposal_id, &canonical_url, &channels, &request.targets)?;
    store::update_proposal(
        conn,
        MutationContext {
            client_id,
            actor_id,
            expected_revision: Some(request.expected_revision),
            idempotency_key: &request.idempotency_key,
            now_ms,
        },
        proposal_id,
        &canonical_url,
        &targets,
    )
}

pub fn approve_request(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    proposal_id: &str,
    expected_revision: u64,
    idempotency_key: &str,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    require_idempotency_key(idempotency_key)?;
    if crate::slices::content_plans::store::social_proposal_campaign_locked(
        conn,
        client_id,
        proposal_id,
    )? {
        return Err(StoreError::Domain(
            "social_proposal_campaign_locked".to_string(),
        ));
    }
    let current = store::get_proposal(conn, client_id, proposal_id)?
        .ok_or_else(|| StoreError::Domain("social_proposal_not_found".to_string()))?;
    if let Some(source_id) = current.proposal.source_id.as_deref() {
        if store::get_source(conn, client_id, source_id)?
            .is_some_and(|source| source.source_kind == PREVIEW_SOURCE_KIND)
        {
            return Err(StoreError::Domain(
                "social_preview_requires_campaign_approval".to_string(),
            ));
        }
    }
    let channels = configured_channels()?;
    validate_snapshot_channels(&current.proposal.targets, &channels)?;
    let jobs = build_channel_jobs(
        client_id,
        &current.proposal,
        actor_id,
        expected_revision,
        now_ms,
    )?;
    store::approve_proposal(
        conn,
        MutationContext {
            client_id,
            actor_id,
            expected_revision: Some(expected_revision),
            idempotency_key,
            now_ms,
        },
        proposal_id,
        &jobs,
    )
}

pub fn reject_request(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    proposal_id: &str,
    expected_revision: u64,
    idempotency_key: &str,
    now_ms: u64,
) -> Result<MutationOutcome, StoreError> {
    require_idempotency_key(idempotency_key)?;
    if crate::slices::content_plans::store::social_proposal_campaign_locked(
        conn,
        client_id,
        proposal_id,
    )? {
        return Err(StoreError::Domain(
            "social_proposal_campaign_locked".to_string(),
        ));
    }
    store::reject_proposal(
        conn,
        MutationContext {
            client_id,
            actor_id,
            expected_revision: Some(expected_revision),
            idempotency_key,
            now_ms,
        },
        proposal_id,
    )
}

pub fn ingest_source_request(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    actor_kind: ActorKindDto,
    request: &SocialPublishedContentIngressRequest,
    now_ms: u64,
) -> Result<SocialPublishedSource, StoreError> {
    require_idempotency_key(&request.idempotency_key)?;
    let source_kind = bounded_identifier(&request.source_kind, 60, "social_source_kind_invalid")?;
    let external_id = bounded_text(&request.external_id, 300, "social_external_id_invalid")?;
    let title = bounded_text(
        &request.title,
        MAX_SOURCE_TITLE_CHARS,
        "social_source_title_invalid",
    )?;
    let excerpt = bounded_optional_text(
        request.excerpt.as_deref(),
        MAX_SOURCE_EXCERPT_CHARS,
        "social_source_excerpt_too_long",
    )?;
    let published_at = match clean_optional(request.published_at.as_deref()) {
        Some(value) => Some(
            crate::slices::datetime_input::normalize_rfc3339_datetime(&value)
                .map_err(|_| StoreError::Domain("social_published_at_invalid".to_string()))?,
        ),
        None => None,
    };
    let canonical_url = normalize_canonical_url(&request.canonical_url)?;
    validate_published_source(
        conn,
        client_id,
        None,
        request.source_content_draft_id.as_deref(),
        None,
        &canonical_url,
    )?;
    let source_id = source_id_for(client_id, &source_kind, &external_id);
    let source = SocialPublishedSource {
        source_id: source_id.clone(),
        source_kind,
        external_id,
        source_content_draft_id: clean_optional(request.source_content_draft_id.as_deref()),
        source_content_draft_revision: None,
        title,
        canonical_url,
        excerpt,
        published_at,
        generation_status: SocialSourceGenerationStatus::Ready,
        generation_run_id: None,
        generation_error: None,
        proposal_id: None,
        revision: 0,
    };
    persist_source_metadata(
        conn,
        client_id,
        actor_id,
        actor_kind,
        &source,
        &request.idempotency_key,
        now_ms,
    )
}

fn persist_source_metadata(
    conn: &mut Connection,
    client_id: &str,
    actor_id: &str,
    actor_kind: ActorKindDto,
    source: &SocialPublishedSource,
    idempotency_key: &str,
    now_ms: u64,
) -> Result<SocialPublishedSource, StoreError> {
    let existing = store::get_source(conn, client_id, &source.source_id)?;
    let (effective, expected_revision) = match existing {
        Some(mut current) => {
            if current.source_kind != source.source_kind
                || current.external_id != source.external_id
                || current.source_content_draft_id != source.source_content_draft_id
                || current.source_content_draft_revision != source.source_content_draft_revision
                || current.canonical_url != source.canonical_url
            {
                return Err(StoreError::Domain(
                    "social_published_source_identity_changed".to_string(),
                ));
            }
            let expected_revision = current.revision;
            current.title = source.title.clone();
            current.excerpt = source.excerpt.clone();
            current.published_at = source.published_at.clone();
            (current, Some(expected_revision))
        }
        None => (source.clone(), None),
    };
    let outcome = store::ingest_source(
        conn,
        MutationContext {
            client_id,
            actor_id,
            expected_revision,
            idempotency_key,
            now_ms,
        },
        actor_kind,
        &effective,
    )?;
    if matches!(outcome, MutationOutcome::RevisionConflict { .. }) {
        return Err(StoreError::Domain(
            "social_published_source_changed".to_string(),
        ));
    }
    store::get_source(conn, client_id, &source.source_id)?
        .ok_or_else(|| StoreError::Domain("social_published_source_not_found".to_string()))
}

#[derive(Debug)]
pub enum GenerationKickoffOutcome {
    Accepted(Box<SocialPublishedSource>),
    Conflict(MutationOutcome),
}

#[derive(Debug)]
enum GenerationKickoffError {
    Store(StoreError),
    Conflict(MutationOutcome),
}

impl From<StoreError> for GenerationKickoffError {
    fn from(value: StoreError) -> Self {
        Self::Store(value)
    }
}

pub fn kickoff_draft_preview_generation(
    state: crate::http::AppState,
    draft_id: &str,
    request: &SocialDraftPreviewGenerateRequest,
    actor_id: &str,
) -> Result<GenerationKickoffOutcome, StoreError> {
    require_idempotency_key(&request.idempotency_key)?;
    configured_channels()?;
    let canonical_url = normalize_canonical_url(&request.expected_canonical_url)?;
    let source = {
        let mut persistence = state.persistence.lock();
        let entry = crate::slices::content_drafts::store::get_draft(
            persistence.connection_ref(),
            &state.client_id,
            draft_id,
        )?
        .ok_or_else(|| StoreError::Domain("content_draft_not_found".to_string()))?;
        if entry.revision != request.expected_content_draft_revision {
            return Err(StoreError::Domain(
                "social_preview_article_revision_changed".to_string(),
            ));
        }
        if !matches!(
            entry.draft.status,
            ContentDraftStatus::Staged | ContentDraftStatus::Approved
        ) {
            return Err(StoreError::Domain(
                "social_preview_article_unavailable".to_string(),
            ));
        }
        let external_id = format!(
            "{}:{}:{}",
            draft_id,
            entry.revision,
            hash_prefix(&canonical_url, 16)
        );
        let source_id = source_id_for(&state.client_id, PREVIEW_SOURCE_KIND, &external_id);
        let source = SocialPublishedSource {
            source_id,
            source_kind: PREVIEW_SOURCE_KIND.to_string(),
            external_id,
            source_content_draft_id: Some(draft_id.to_string()),
            source_content_draft_revision: Some(entry.revision),
            title: entry.draft.title,
            canonical_url,
            excerpt: entry.draft.meta_description,
            published_at: None,
            generation_status: SocialSourceGenerationStatus::Ready,
            generation_run_id: None,
            generation_error: None,
            proposal_id: None,
            revision: 0,
        };
        persist_source_metadata(
            persistence.connection(),
            &state.client_id,
            actor_id,
            ActorKindDto::Operator,
            &source,
            &format!("social-preview-source:{}", request.idempotency_key),
            crate::http::now_ms(),
        )?
    };
    kickoff_generation(
        state,
        &source.source_id,
        source.revision,
        &format!("preview:{}", request.idempotency_key),
        actor_id,
        ActorKindDto::Operator,
    )
}

pub fn kickoff_generation(
    state: crate::http::AppState,
    source_id: &str,
    expected_revision: u64,
    idempotency_key: &str,
    actor_id: &str,
    actor_kind: ActorKindDto,
) -> Result<GenerationKickoffOutcome, StoreError> {
    require_idempotency_key(idempotency_key)?;
    let (source, synthetic) = ensure_source_registered(
        &state,
        source_id,
        expected_revision,
        idempotency_key,
        crate::http::now_ms(),
    )?;
    let effective_expected = if synthetic {
        source.revision
    } else {
        expected_revision
    };
    let run_id = format!(
        "socialgen_{}",
        hash_prefix(
            &format!("{}:{}:{}", state.client_id, source_id, idempotency_key),
            24,
        )
    );
    if source.revision == effective_expected
        && (source.generation_status == SocialSourceGenerationStatus::ProposalStaged
            || (source.generation_status == SocialSourceGenerationStatus::Generating
                && source.generation_run_id.as_deref() != Some(run_id.as_str())))
    {
        return Ok(GenerationKickoffOutcome::Accepted(Box::new(source)));
    }
    let start_key = format!("social-generation-start:{idempotency_key}");
    let decision = crate::slices::async_kickoff::begin(
        KickoffSpec {
            slice_id: "social_publishing",
            draft_id: source_id,
            planned_run_id: &run_id,
            capacity: KickoffCapacity::Limited {
                group: "social_publishing_generation",
                max_concurrent: 2,
            },
        },
        || {
            let mut persistence = state.persistence.lock();
            let current =
                store::get_source(persistence.connection_ref(), &state.client_id, source_id)?
                    .ok_or_else(|| {
                        StoreError::Domain("social_published_source_not_found".to_string())
                    })?;
            if current.generation_run_id.as_deref() == Some(&run_id)
                && current.generation_status == SocialSourceGenerationStatus::ProposalStaged
            {
                return Ok(RecordedKickoff {
                    run_id: run_id.clone(),
                    replayed: true,
                });
            }
            if current.generation_run_id.as_deref() == Some(&run_id)
                && current.generation_status == SocialSourceGenerationStatus::Generating
            {
                // Same-process duplicates are stopped by async_kickoff before
                // this closure. Reaching here means a durable run survived a
                // process restart, so resume that exact run.
                return Ok(RecordedKickoff {
                    run_id: run_id.clone(),
                    replayed: false,
                });
            }
            let outcome = store::begin_generation(
                persistence.connection(),
                MutationContext {
                    client_id: &state.client_id,
                    actor_id,
                    expected_revision: Some(effective_expected),
                    idempotency_key: &start_key,
                    now_ms: crate::http::now_ms(),
                },
                actor_kind,
                source_id,
                &run_id,
            )?;
            match outcome {
                MutationOutcome::RevisionConflict { .. } => {
                    Err(GenerationKickoffError::Conflict(outcome))
                }
                MutationOutcome::ReplayedIdempotent { .. } => Ok(RecordedKickoff {
                    run_id: run_id.clone(),
                    replayed: true,
                }),
                MutationOutcome::Applied { .. } => Ok(RecordedKickoff {
                    run_id: run_id.clone(),
                    replayed: false,
                }),
            }
        },
    );
    let decision = match decision {
        Ok(decision) => decision,
        Err(GenerationKickoffError::Conflict(outcome)) => {
            return Ok(GenerationKickoffOutcome::Conflict(outcome));
        }
        Err(GenerationKickoffError::Store(err)) => return Err(err),
    };
    if let KickoffDecision::CapacityExceeded = decision {
        return Err(StoreError::Domain(
            "social_generation_capacity_exceeded".to_string(),
        ));
    }
    if let KickoffDecision::Spawn { guard, .. } = decision {
        let worker_state = state.clone();
        let worker_source_id = source_id.to_string();
        let worker_run_id = run_id.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("social-draft-{source_id}"))
            .spawn(move || {
                let _guard = guard;
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    execute_source_generation(&worker_state, &worker_source_id, &worker_run_id)
                }));
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => {
                        tracing::warn!(source_id = %worker_source_id, error = %err, "social draft generation failed");
                    }
                    Err(_) => {
                        let _ = finish_generation_failed(
                            &worker_state,
                            &worker_source_id,
                            &worker_run_id,
                            "social_generation_panicked",
                        );
                        tracing::error!(source_id = %worker_source_id, "social draft generation panicked");
                    }
                }
            });
        if spawned.is_err() {
            finish_generation_failed(&state, source_id, &run_id, "social_generation_spawn_failed")?;
            return Err(StoreError::Domain(
                "social_generation_spawn_failed".to_string(),
            ));
        }
    }
    let persistence = state.persistence.lock();
    let current = store::get_source(persistence.connection_ref(), &state.client_id, source_id)?
        .ok_or_else(|| StoreError::Domain("social_published_source_not_found".to_string()))?;
    Ok(GenerationKickoffOutcome::Accepted(Box::new(current)))
}

fn ensure_source_registered(
    state: &crate::http::AppState,
    source_id: &str,
    expected_revision: u64,
    idempotency_key: &str,
    now_ms: u64,
) -> Result<(SocialPublishedSource, bool), StoreError> {
    let mut persistence = state.persistence.lock();
    if let Some(source) =
        store::get_source(persistence.connection_ref(), &state.client_id, source_id)?
    {
        return Ok((source, false));
    }
    if expected_revision != 0 {
        return Err(StoreError::Domain(
            "social_published_source_not_found".to_string(),
        ));
    }
    let source = published_sources(persistence.connection_ref(), &state.client_id)?
        .into_iter()
        .find(|source| source.source_id == source_id)
        .ok_or_else(|| StoreError::Domain("social_published_source_not_found".to_string()))?;
    let registration_key = format!("social-source-register:{idempotency_key}");
    store::ingest_source(
        persistence.connection(),
        MutationContext {
            client_id: &state.client_id,
            actor_id: GENERATOR_ACTOR,
            expected_revision: None,
            idempotency_key: &registration_key,
            now_ms,
        },
        ActorKindDto::System,
        &source,
    )?;
    let source = store::get_source(persistence.connection_ref(), &state.client_id, source_id)?
        .ok_or_else(|| StoreError::Domain("social_published_source_not_found".to_string()))?;
    Ok((source, true))
}

fn execute_source_generation(
    state: &crate::http::AppState,
    source_id: &str,
    run_id: &str,
) -> Result<(), StoreError> {
    let (source, grounding, channels) = {
        let persistence = state.persistence.lock();
        let source = store::get_source(persistence.connection_ref(), &state.client_id, source_id)?
            .ok_or_else(|| StoreError::Domain("social_published_source_not_found".to_string()))?;
        if source.generation_status != SocialSourceGenerationStatus::Generating
            || source.generation_run_id.as_deref() != Some(run_id)
        {
            return Ok(());
        }
        let grounding = source_grounding(persistence.connection_ref(), &state.client_id, &source)?;
        (source, grounding, configured_channels()?)
    };

    let mut last_code = "social_draft_output_invalid".to_string();
    for attempt in 1..=MAX_GENERATION_ATTEMPTS {
        let request = build_social_draft_request(
            &state.client_id,
            &source,
            &channels,
            &grounding,
            run_id,
            attempt,
        );
        let envelope = match execute_social_draft_llm(state, &request) {
            Ok(envelope) => envelope,
            Err(code) => {
                last_code = code;
                continue;
            }
        };
        let targets =
            match parse_social_draft_response(&envelope.response_json, &channels, &grounding) {
                Ok(targets) => targets,
                Err(code) => {
                    last_code = code;
                    continue;
                }
            };
        let request = SocialProposalStageRequest {
            source_id: Some(source.source_id.clone()),
            source_content_draft_id: source.source_content_draft_id.clone(),
            source_content_draft_revision: source.source_content_draft_revision,
            canonical_url: source.canonical_url.clone(),
            targets,
            idempotency_key: format!("social-generation-stage:{run_id}"),
            actor_id: None,
        };
        let proposal_id = {
            let mut persistence = state.persistence.lock();
            match stage_request(
                persistence.connection(),
                &state.client_id,
                GENERATOR_ACTOR,
                ActorKindDto::System,
                &request,
                crate::http::now_ms(),
            ) {
                Ok((_, proposal_id)) => proposal_id,
                Err(err) => {
                    last_code = store_error_code(&err);
                    continue;
                }
            }
        };
        finish_generation_staged(state, source_id, run_id, &proposal_id)?;
        return Ok(());
    }
    finish_generation_failed(state, source_id, run_id, &last_code)?;
    Err(StoreError::Domain(last_code))
}

fn finish_generation_staged(
    state: &crate::http::AppState,
    source_id: &str,
    run_id: &str,
    proposal_id: &str,
) -> Result<(), StoreError> {
    finish_generation_state(state, source_id, run_id, Some(proposal_id), None)
}

fn finish_generation_failed(
    state: &crate::http::AppState,
    source_id: &str,
    run_id: &str,
    error_code: &str,
) -> Result<(), StoreError> {
    finish_generation_state(state, source_id, run_id, None, Some(error_code))
}

fn finish_generation_state(
    state: &crate::http::AppState,
    source_id: &str,
    run_id: &str,
    proposal_id: Option<&str>,
    error_code: Option<&str>,
) -> Result<(), StoreError> {
    let mut persistence = state.persistence.lock();
    let current = store::get_source(persistence.connection_ref(), &state.client_id, source_id)?
        .ok_or_else(|| StoreError::Domain("social_published_source_not_found".to_string()))?;
    let outcome_key = format!("social-generation-finish:{run_id}");
    store::finish_generation(
        persistence.connection(),
        MutationContext {
            client_id: &state.client_id,
            actor_id: GENERATOR_ACTOR,
            expected_revision: Some(current.revision),
            idempotency_key: &outcome_key,
            now_ms: crate::http::now_ms(),
        },
        source_id,
        run_id,
        proposal_id,
        error_code,
    )?;
    Ok(())
}

fn source_grounding(
    conn: &Connection,
    client_id: &str,
    source: &SocialPublishedSource,
) -> Result<String, StoreError> {
    let mut blocks = vec![source.title.clone()];
    if let Some(excerpt) = source.excerpt.as_deref() {
        blocks.push(excerpt.to_string());
    }
    if let Some(draft_id) = source.source_content_draft_id.as_deref() {
        let draft = crate::slices::content_drafts::store::get_draft(conn, client_id, draft_id)?
            .ok_or_else(|| StoreError::Domain("social_published_source_not_found".to_string()))?;
        if source.source_content_draft_revision.is_some()
            && source.source_content_draft_revision != Some(draft.revision)
        {
            return Err(StoreError::Domain(
                "social_preview_article_revision_changed".to_string(),
            ));
        }
        blocks.push(draft.draft.body_markdown);
    }
    Ok(blocks
        .join("\n\n")
        .chars()
        .take(MAX_SOURCE_BODY_CHARS)
        .collect())
}

pub fn build_social_draft_request(
    client_id: &str,
    source: &SocialPublishedSource,
    channels: &[SocialPublishingChannel],
    grounding: &str,
    run_id: &str,
    attempt: u64,
) -> TypedLlmTaskRequest {
    let task_id = format!("{run_id}_{attempt}");
    let channel_prompts = channels
        .iter()
        .enumerate()
        .map(|(index, channel)| {
            json!({
                "target_ref": format!("target_{}", index + 1),
                "name": channel.name,
                "platform": channel.platform,
            })
        })
        .collect::<Vec<_>>();
    TypedLlmTaskRequest {
        task_id: task_id.clone(),
        correlation_id: source.source_id.clone(),
        idempotency_key: task_id,
        tenant_or_project_scope: client_id.to_string(),
        source_entity: Some(TypedLlmSourceEntity {
            entity_kind: store::SOURCE_ENTITY_KIND.to_string(),
            entity_id: source.source_id.clone(),
        }),
        spec: TypedLlmTaskSpec {
            task_class: TypedLlmTaskClass::Draft,
            prompt_template_id: "social_campaign_draft".to_string(),
            prompt_template_version: "1".to_string(),
            prompt_template_hash: String::new(),
            schema_ref: DRAFT_SCHEMA_REF.to_string(),
            response_format: TypedLlmResponseFormat::JsonObject,
            max_input_bytes: 64 * 1024,
            max_output_bytes: 32 * 1024,
            max_tokens: 0,
            timeout_ms: 0,
            capabilities: TypedLlmTaskCapabilities::pure_transformation(),
            authority: TypedLlmAuthority::no_side_effects(),
        },
        input: TypedLlmTaskInput {
            json: json!({
                "instructions": "Draft one grounded social post per target_ref. Use only facts in SOURCE CONTENT. Return exactly {targets:[{target_ref,text,utm_source,utm_medium,utm_campaign,utm_content,source_quotes}],confidence}. source_quotes must contain literal spans from SOURCE CONTENT supporting the copy. Do not include channel IDs, credentials, approval actions, schedules, or provider instructions.",
                "canonical_url": source.canonical_url,
                "title": source.title,
                "published_at": source.published_at,
                "channels": channel_prompts,
            }),
            text_blocks: vec![TypedLlmTextBlock {
                block_id: "source_content".to_string(),
                text: grounding.to_string(),
            }],
        },
        execution_policy: TypedLlmExecutionPolicy {
            default_route: TypedLlmExecutionRoute::DirectApi,
            fallback_policy: TypedLlmFallbackPolicy::NoFallback,
            retry_policy: TypedLlmRetryPolicy {
                max_attempts: 2,
                backoff_ms: 1_000,
                max_elapsed_ms: 180_000,
            },
        },
        provider_policy: TypedLlmProviderPolicy {
            preferred_provider: String::new(),
            preferred_model: String::new(),
            fallback_provider: None,
            fallback_model: None,
        },
        safety_policy: TypedLlmSafetyPolicy {
            redaction_policy: TypedLlmRedactionPolicy::PreSubmit,
            raw_output_retention:
                bos_integrations::llm_typed_tasks::TypedLlmRawOutputRetention::None,
        },
    }
}

pub fn parse_social_draft_response(
    response: &Value,
    channels: &[SocialPublishingChannel],
    grounding: &str,
) -> Result<Vec<SocialProposalTargetInput>, String> {
    let output: SocialDraftOutput = serde_json::from_value(response.clone())
        .map_err(|_| "social_draft_output_invalid".to_string())?;
    if !matches!(output.confidence.as_str(), "high" | "medium" | "low") {
        return Err("social_draft_confidence_invalid".to_string());
    }
    let raw_targets = output
        .targets
        .len()
        .eq(&channels.len())
        .then_some(output.targets)
        .ok_or_else(|| "social_draft_target_set_invalid".to_string())?;
    let grounding_lower = grounding.to_lowercase();
    let mut by_ref = BTreeMap::new();
    for target in raw_targets {
        let target_ref = required_draft_text(&target.target_ref, "target_ref")?;
        if by_ref.insert(target_ref, target).is_some() {
            return Err("social_draft_target_set_invalid".to_string());
        }
    }
    channels
        .iter()
        .enumerate()
        .map(|(index, channel)| {
            let target_ref = format!("target_{}", index + 1);
            let target = by_ref
                .get(&target_ref)
                .ok_or_else(|| "social_draft_target_set_invalid".to_string())?;
            let text = required_draft_text(&target.text, "text")?;
            if target.source_quotes.is_empty() || target.source_quotes.len() > 8 {
                return Err("social_draft_grounding_missing".to_string());
            }
            for quote in &target.source_quotes {
                let quote = Some(quote.trim())
                    .filter(|quote| (5..=500).contains(&quote.chars().count()))
                    .ok_or_else(|| "social_draft_grounding_invalid".to_string())?;
                if !grounding_lower.contains(&quote.to_lowercase()) {
                    return Err("social_draft_grounding_invalid".to_string());
                }
            }
            Ok(SocialProposalTargetInput {
                channel_id: channel.channel_id.clone(),
                text,
                image_url: None,
                utm: SocialUtmParameters {
                    source: Some(required_draft_text(&target.utm_source, "utm_source")?),
                    medium: Some(required_draft_text(&target.utm_medium, "utm_medium")?),
                    campaign: Some(required_draft_text(&target.utm_campaign, "utm_campaign")?),
                    content: clean_optional(target.utm_content.as_deref()),
                },
                schedule_mode: SocialScheduleMode::Queue,
                due_at: None,
            })
        })
        .collect()
}

fn execute_social_draft_llm(
    state: &crate::http::AppState,
    request: &TypedLlmTaskRequest,
) -> Result<TypedLlmTaskOutputEnvelope, String> {
    #[cfg(test)]
    if let Some(response_json) = take_test_social_draft_response() {
        return Ok(TypedLlmTaskOutputEnvelope {
            task_id: request.task_id.clone(),
            execution_route: TypedLlmExecutionRoute::DirectApi,
            provider_id: "test".to_string(),
            model: "test-model".to_string(),
            schema_ref: request.spec.schema_ref.clone(),
            raw_response_hash: "test".to_string(),
            response_json,
            usage: None,
            finish_reason: Some("stop".to_string()),
            latency_ms: 0,
            retry_count: 0,
            provider_request_id: None,
            correlation_id: request.correlation_id.clone(),
        });
    }
    crate::slices::ai_usage::service::execute_recorded(
        state.persistence.clone(),
        &state.client_id,
        DRAFT_PURPOSE,
        request,
    )
    .map_err(|err| err.code().to_string())
}

#[cfg(test)]
pub(crate) fn set_test_social_draft_responses(responses: Vec<Value>) {
    *TEST_SOCIAL_DRAFT_RESPONSES
        .get_or_init(|| Mutex::new(VecDeque::new()))
        .lock()
        .expect("social draft test response mutex") = responses.into();
}

#[cfg(test)]
fn take_test_social_draft_response() -> Option<Value> {
    TEST_SOCIAL_DRAFT_RESPONSES
        .get_or_init(|| Mutex::new(VecDeque::new()))
        .lock()
        .expect("social draft test response mutex")
        .pop_front()
}

#[cfg(test)]
static TEST_SOCIAL_DRAFT_RESPONSES: OnceLock<Mutex<VecDeque<Value>>> = OnceLock::new();

pub fn build_channel_jobs(
    client_id: &str,
    proposal: &SocialPostProposal,
    approved_by: &str,
    approved_revision: u64,
    approved_at_ms: u64,
) -> Result<Vec<NewOutboxJob>, StoreError> {
    if approved_revision == 0 || approved_by.trim().is_empty() || proposal.targets.is_empty() {
        return Err(StoreError::Domain(
            "social_approval_snapshot_invalid".to_string(),
        ));
    }
    proposal
        .targets
        .iter()
        .map(|target| {
            let channel_key = format!(
                "social:{}:{}:{}",
                proposal.proposal_id, approved_revision, target.channel_id
            );
            let payload = BufferPostOutboxPayload {
                schema_version: 1,
                client_id: client_id.to_string(),
                proposal_id: proposal.proposal_id.clone(),
                target_id: target.target_id.clone(),
                channel_id: target.channel_id.clone(),
                channel_name: target.channel_name.clone(),
                platform: target.platform.clone(),
                canonical_url: proposal.canonical_url.clone(),
                tracked_url: target.tracked_url.clone(),
                text: target.text.clone(),
                image_url: target.image_url.clone(),
                utm_json: serde_json::to_string(&target.utm).map_err(|err| {
                    StoreError::Domain(format!("serialize social UTM snapshot: {err}"))
                })?,
                schedule_mode: match target.schedule_mode {
                    SocialScheduleMode::Queue => BufferScheduleMode::Queue,
                    SocialScheduleMode::Scheduled => BufferScheduleMode::Scheduled,
                },
                due_at: target.due_at.clone(),
                approval: BufferApprovalMetadata {
                    approved_by: approved_by.to_string(),
                    approved_at_ms,
                    approved_revision,
                },
                idempotency_key: channel_key.clone(),
            };
            buffer::validate_payload(&payload).map_err(buffer_error_to_store)?;
            let payload_json = serde_json::to_string(&payload).map_err(|err| {
                StoreError::Domain(format!("serialize Buffer post payload: {err}"))
            })?;
            let suffix = hash_prefix(&channel_key, 16);
            Ok(NewOutboxJob {
                job_id: format!("social_{}_{}", proposal.proposal_id, suffix),
                provider: PROVIDER_BUFFER.to_string(),
                capability: CAPABILITY_CREATE_POST.to_string(),
                payload_json,
                source_entity_kind: store::TARGET_ENTITY_KIND.to_string(),
                source_entity_id: target.target_id.clone(),
                correlation_id: Some(proposal.proposal_id.clone()),
                causation_id: Some(format!(
                    "social_approval:{}:{}",
                    proposal.proposal_id, approved_revision
                )),
                idempotency_key: format!("outbox:{channel_key}"),
            })
        })
        .collect()
}

pub fn deliver(state: &crate::http::AppState, job: &ClaimedJob, now_ms: u64) -> AttemptOutcome {
    if job.provider != PROVIDER_BUFFER || job.capability != CAPABILITY_CREATE_POST {
        return AttemptOutcome::Terminal {
            error: format!("outbox_unsupported_job:{}:{}", job.provider, job.capability),
            result_json: None,
        };
    }
    let write_enabled = {
        let persistence = state.persistence.lock();
        buffer_live_enabled(persistence.connection_ref(), &state.client_id)
    };
    let config = BufferWriteConfig {
        api_url: env_registry::string(&env_registry::BOS_BUFFER_API_URL)
            .unwrap_or_else(|| buffer::DEFAULT_BUFFER_API_URL.to_string()),
        access_token: env_registry::string(&env_registry::BOS_BUFFER_ACCESS_TOKEN),
        write_enabled,
    };
    execute_job(job, &config, now_ms)
}

pub fn execute_job(job: &ClaimedJob, config: &BufferWriteConfig, now_ms: u64) -> AttemptOutcome {
    let payload: BufferPostOutboxPayload = match serde_json::from_str(&job.payload_json) {
        Ok(payload) => payload,
        Err(err) => {
            return AttemptOutcome::Terminal {
                error: format!("buffer_payload_invalid:{err}"),
                result_json: None,
            }
        }
    };
    let client = match buffer::buffer_execution_client(config) {
        Ok(client) => client,
        Err(err) => return buffer_error_outcome(err, job.attempts, now_ms),
    };
    match client.create_post(&payload) {
        Ok(result) => AttemptOutcome::Delivered {
            result_json: serde_json::json!({
                "dry_run": result.dry_run,
                "executed": result.executed,
                "provider_object_id": result.post_id,
                "channel_id": payload.channel_id,
                "status": result.status,
                "due_at": result.due_at,
                "approved_revision": payload.approval.approved_revision,
            })
            .to_string(),
        },
        Err(err) => buffer_error_outcome(err, job.attempts, now_ms),
    }
}

fn normalize_targets(
    proposal_id: &str,
    canonical_url: &str,
    channels: &[SocialPublishingChannel],
    inputs: &[SocialProposalTargetInput],
) -> Result<Vec<SocialProposalTarget>, StoreError> {
    if inputs.len() != channels.len() {
        return Err(StoreError::Domain(
            "social_channel_set_incomplete".to_string(),
        ));
    }
    let mut by_channel = BTreeMap::new();
    for input in inputs {
        let channel_id = input.channel_id.trim();
        if channel_id.is_empty() || by_channel.insert(channel_id.to_string(), input).is_some() {
            return Err(StoreError::Domain("social_channel_set_invalid".to_string()));
        }
    }
    channels
        .iter()
        .map(|channel| {
            let input = by_channel
                .get(&channel.channel_id)
                .ok_or_else(|| StoreError::Domain("social_channel_set_incomplete".to_string()))?;
            normalize_target(proposal_id, canonical_url, channel, input)
        })
        .collect()
}

fn normalize_target(
    proposal_id: &str,
    canonical_url: &str,
    channel: &SocialPublishingChannel,
    input: &SocialProposalTargetInput,
) -> Result<SocialProposalTarget, StoreError> {
    let utm = normalize_utm(&input.utm)?;
    let tracked_url = tracked_url(canonical_url, &utm)?;
    let raw_text = input.text.trim();
    if raw_text.is_empty() {
        return Err(StoreError::Domain("social_post_text_required".to_string()));
    }
    let text = if raw_text.contains(&tracked_url) {
        raw_text.to_string()
    } else if raw_text.contains(canonical_url) {
        raw_text.replacen(canonical_url, &tracked_url, 1)
    } else {
        format!("{raw_text}\n\n{tracked_url}")
    };
    if text.chars().count() > MAX_POST_TEXT_CHARS {
        return Err(StoreError::Domain("social_post_text_too_long".to_string()));
    }
    let image_url = match clean_optional(input.image_url.as_deref()) {
        Some(url) => {
            require_https_url(&url, "social_image_url_invalid")?;
            Some(url)
        }
        None => None,
    };
    let (schedule_mode, due_at) = match input.schedule_mode {
        SocialScheduleMode::Queue => {
            if clean_optional(input.due_at.as_deref()).is_some() {
                return Err(StoreError::Domain(
                    "social_queue_due_at_invalid".to_string(),
                ));
            }
            (SocialScheduleMode::Queue, None)
        }
        SocialScheduleMode::Scheduled => {
            let due_at = clean_optional(input.due_at.as_deref())
                .ok_or_else(|| StoreError::Domain("social_schedule_due_at_required".to_string()))?;
            let due_at = crate::slices::datetime_input::normalize_rfc3339_datetime(&due_at)
                .map_err(|_| StoreError::Domain("social_schedule_due_at_invalid".to_string()))?;
            (SocialScheduleMode::Scheduled, Some(due_at))
        }
    };
    Ok(SocialProposalTarget {
        target_id: format!(
            "spt_{}",
            hash_prefix(&format!("{proposal_id}:{}", channel.channel_id), 24)
        ),
        channel_id: channel.channel_id.clone(),
        channel_name: channel.name.clone(),
        platform: channel.platform.clone(),
        text,
        tracked_url,
        image_url,
        utm,
        schedule_mode,
        due_at,
        outbox_job_id: None,
        outbox_job: None,
    })
}

fn normalize_utm(raw: &SocialUtmParameters) -> Result<SocialUtmParameters, StoreError> {
    let utm = SocialUtmParameters {
        source: bounded_optional(raw.source.as_deref(), "utm_source")?,
        medium: bounded_optional(raw.medium.as_deref(), "utm_medium")?,
        campaign: bounded_optional(raw.campaign.as_deref(), "utm_campaign")?,
        content: bounded_optional(raw.content.as_deref(), "utm_content")?,
    };
    let primary_count = [
        utm.source.as_ref(),
        utm.medium.as_ref(),
        utm.campaign.as_ref(),
    ]
    .into_iter()
    .flatten()
    .count();
    if primary_count != 0 && primary_count != 3 {
        return Err(StoreError::Domain(
            "social_utm_parameters_incomplete".to_string(),
        ));
    }
    if primary_count == 0 && utm.content.is_some() {
        return Err(StoreError::Domain(
            "social_utm_parameters_incomplete".to_string(),
        ));
    }
    Ok(utm)
}

fn tracked_url(canonical_url: &str, utm: &SocialUtmParameters) -> Result<String, StoreError> {
    let mut url = url::Url::parse(canonical_url)
        .map_err(|_| StoreError::Domain("social_canonical_url_invalid".to_string()))?;
    let preserved = url
        .query_pairs()
        .filter(|(key, _)| {
            !matches!(
                key.as_ref(),
                "utm_source" | "utm_medium" | "utm_campaign" | "utm_content"
            )
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();
    url.set_query(None);
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in preserved {
            pairs.append_pair(&key, &value);
        }
        for (key, value) in [
            ("utm_source", utm.source.as_deref()),
            ("utm_medium", utm.medium.as_deref()),
            ("utm_campaign", utm.campaign.as_deref()),
            ("utm_content", utm.content.as_deref()),
        ] {
            if let Some(value) = value {
                pairs.append_pair(key, value);
            }
        }
    }
    Ok(url.to_string())
}

pub(crate) fn normalize_canonical_url(raw: &str) -> Result<String, StoreError> {
    let mut url = url::Url::parse(raw.trim())
        .map_err(|_| StoreError::Domain("social_canonical_url_invalid".to_string()))?;
    if url.scheme() != "https" || url.host_str().is_none() {
        return Err(StoreError::Domain(
            "social_canonical_url_invalid".to_string(),
        ));
    }
    url.set_fragment(None);
    Ok(url.to_string())
}

fn validate_published_source(
    conn: &Connection,
    client_id: &str,
    source_id: Option<&str>,
    source_content_draft_id: Option<&str>,
    source_content_draft_revision: Option<u64>,
    canonical_url: &str,
) -> Result<(), StoreError> {
    let Some(draft_id) = source_content_draft_id
        .map(str::trim)
        .filter(|draft_id| !draft_id.is_empty())
    else {
        return Ok(());
    };
    let entry = crate::slices::content_drafts::store::get_draft(conn, client_id, draft_id)?
        .ok_or_else(|| StoreError::Domain("social_published_source_not_found".to_string()))?;
    if let Some(source_id) = source_id {
        if let Some(source) = store::get_source(conn, client_id, source_id)? {
            if source.source_kind == PREVIEW_SOURCE_KIND {
                if source.source_content_draft_id.as_deref() != Some(draft_id)
                    || source.source_content_draft_revision != source_content_draft_revision
                    || source.source_content_draft_revision != Some(entry.revision)
                    || source.canonical_url != canonical_url
                    || !matches!(
                        entry.draft.status,
                        ContentDraftStatus::Staged | ContentDraftStatus::Approved
                    )
                {
                    return Err(StoreError::Domain(
                        "social_preview_article_revision_changed".to_string(),
                    ));
                }
                return Ok(());
            }
        }
    }
    let job = entry
        .outbox_job
        .ok_or_else(|| StoreError::Domain("social_published_source_not_live".to_string()))?;
    let published_url = job
        .provider_object_id
        .ok_or_else(|| StoreError::Domain("social_published_source_not_live".to_string()))?;
    if entry.draft.status != ContentDraftStatus::Approved
        || job.status != crate::outbox::STATUS_DELIVERED
        || job.dry_run == Some(true)
    {
        return Err(StoreError::Domain(
            "social_published_source_not_live".to_string(),
        ));
    }
    let normalized_published_url = normalize_canonical_url(&published_url)?;
    if normalized_published_url != canonical_url {
        return Err(StoreError::Domain(
            "social_published_source_url_mismatch".to_string(),
        ));
    }
    Ok(())
}

fn validate_registered_source(
    conn: &Connection,
    client_id: &str,
    source_id: Option<&str>,
    source_content_draft_id: Option<&str>,
    source_content_draft_revision: Option<u64>,
    canonical_url: &str,
) -> Result<(), StoreError> {
    let Some(source_id) = source_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let source = match store::get_source(conn, client_id, source_id)? {
        Some(source) => source,
        None => {
            let Some(draft_id) = source_content_draft_id
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                return Err(StoreError::Domain(
                    "social_published_source_not_found".to_string(),
                ));
            };
            if source_id == source_id_for(client_id, "businessos_content", draft_id) {
                return Ok(());
            }
            return Err(StoreError::Domain(
                "social_published_source_not_found".to_string(),
            ));
        }
    };
    if source.canonical_url != canonical_url
        || clean_optional(source.source_content_draft_id.as_deref())
            != clean_optional(source_content_draft_id)
        || source.source_content_draft_revision != source_content_draft_revision
    {
        return Err(StoreError::Domain(
            "social_published_source_url_mismatch".to_string(),
        ));
    }
    Ok(())
}

fn validate_snapshot_channels(
    targets: &[SocialProposalTarget],
    channels: &[SocialPublishingChannel],
) -> Result<(), StoreError> {
    if targets.len() != channels.len()
        || targets.iter().zip(channels).any(|(target, channel)| {
            target.channel_id != channel.channel_id
                || target.channel_name != channel.name
                || target.platform != channel.platform
        })
    {
        return Err(StoreError::Domain(
            "social_channel_configuration_changed".to_string(),
        ));
    }
    Ok(())
}

fn proposal_id_for(client_id: &str, idempotency_key: &str) -> String {
    format!(
        "social_{}",
        hash_prefix(&format!("{client_id}:{idempotency_key}"), 24)
    )
}

fn source_id_for(client_id: &str, source_kind: &str, external_id: &str) -> String {
    format!(
        "socialsrc_{}",
        hash_prefix(&format!("{client_id}:{source_kind}:{external_id}"), 24)
    )
}

fn hash_prefix(value: &str, chars: usize) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
        .chars()
        .take(chars)
        .collect()
}

fn valid_platform(raw: &str) -> bool {
    buffer::supports_platform(raw)
}

fn clean_optional(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn bounded_text(raw: &str, max_chars: usize, code: &str) -> Result<String, StoreError> {
    let value = raw.trim();
    if value.is_empty() || value.chars().count() > max_chars {
        return Err(StoreError::Domain(code.to_string()));
    }
    Ok(value.to_string())
}

fn bounded_identifier(raw: &str, max_chars: usize, code: &str) -> Result<String, StoreError> {
    let value = bounded_text(raw, max_chars, code)?;
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return Err(StoreError::Domain(code.to_string()));
    }
    Ok(value)
}

fn bounded_optional_text(
    raw: Option<&str>,
    max_chars: usize,
    code: &str,
) -> Result<Option<String>, StoreError> {
    let value = clean_optional(raw);
    if value
        .as_ref()
        .is_some_and(|value| value.chars().count() > max_chars)
    {
        return Err(StoreError::Domain(code.to_string()));
    }
    Ok(value)
}

fn required_draft_text(value: &str, field: &str) -> Result<String, String> {
    Some(value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("social_draft_{field}_invalid"))
}

fn store_error_code(error: &StoreError) -> String {
    match error {
        StoreError::Domain(code) => code.clone(),
        StoreError::Sqlite(_) => "social_generation_storage_failed".to_string(),
    }
}

fn bounded_optional(raw: Option<&str>, field: &str) -> Result<Option<String>, StoreError> {
    let value = clean_optional(raw);
    if value
        .as_ref()
        .is_some_and(|value| value.chars().count() > MAX_UTM_VALUE_CHARS)
    {
        return Err(StoreError::Domain(format!("social_{field}_too_long")));
    }
    Ok(value)
}

fn require_https_url(raw: &str, code: &str) -> Result<(), StoreError> {
    let valid = url::Url::parse(raw)
        .ok()
        .is_some_and(|url| url.scheme() == "https" && url.host_str().is_some());
    if valid {
        Ok(())
    } else {
        Err(StoreError::Domain(code.to_string()))
    }
}

fn require_idempotency_key(idempotency_key: &str) -> Result<(), StoreError> {
    if idempotency_key.trim().is_empty() {
        Err(StoreError::Domain("idempotency_key_required".to_string()))
    } else {
        Ok(())
    }
}

fn buffer_error_to_store(err: BufferWriteError) -> StoreError {
    let code = match err {
        BufferWriteError::Retryable { code, .. }
        | BufferWriteError::Permanent { code, .. }
        | BufferWriteError::OutcomeUnknown { code, .. } => code,
    };
    StoreError::Domain(code)
}

fn buffer_error_outcome(err: BufferWriteError, attempts: u32, now_ms: u64) -> AttemptOutcome {
    match err {
        BufferWriteError::Retryable {
            code,
            retry_after_secs,
            ..
        } => AttemptOutcome::Retry {
            error: code,
            retry_at_ms: retry_after_secs
                .map(|seconds| now_ms.saturating_add(seconds.saturating_mul(1_000)))
                .unwrap_or_else(|| now_ms.saturating_add(retry_backoff_ms(attempts))),
        },
        BufferWriteError::Permanent { code, .. } => AttemptOutcome::Terminal {
            error: code,
            result_json: None,
        },
        BufferWriteError::OutcomeUnknown { code, .. } => AttemptOutcome::OutcomeUnknown {
            error: code,
            result_json: Some(
                serde_json::json!({
                    "delivery_outcome_unknown": true,
                    "manual_reconciliation_required": true,
                    "provider": "buffer",
                })
                .to_string(),
            ),
        },
    }
}
