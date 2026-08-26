//! Consent-gated call inputs: selected call logs, transcripts, and recording
//! references become auditable source items before normal queue work.

pub mod routes;
pub mod service;
pub mod store;
pub mod worker;

#[cfg(test)]
mod tests;

use crate::env_registry;
use crate::slices::{RouteSpec, SliceSpec};

pub const SOURCE_KIND_CALL_INPUT: &str = "call_input";

pub const SLICE: SliceSpec = SliceSpec {
    id: "call_inputs",
    title: "Call inputs",
    summary: "Consent-gated call-log, transcript, and selected recording source inputs. Configured sources require an enabled source with a recorded consent basis before operators can stage inputs; accepted inputs become normal queue items for existing CRM, follow-up, calendar, and email draft flows.",
    routes: &[
        RouteSpec {
            method: "GET",
            path: "/api/call-inputs/status",
            summary: "Configured call input sources and pending consent/fit state",
        },
        RouteSpec {
            method: "GET",
            path: "/api/call-inputs/drive-settings",
            summary: "Configured Google Drive audio intake folder",
        },
        RouteSpec {
            method: "POST",
            path: "/api/call-inputs/drive-settings",
            summary: "Replace the Google Drive audio intake folder setting",
        },
        RouteSpec {
            method: "GET",
            path: "/api/call-inputs",
            summary: "Call inputs newest-first (?status=staged|accepted|rejected)",
        },
        RouteSpec {
            method: "POST",
            path: "/api/call-inputs",
            summary: "Stage one selected call log/transcript/recording reference from an enabled source with a recorded consent basis",
        },
        RouteSpec {
            method: "POST",
            path: "/api/call-inputs/{call_input_id}/action",
            summary: "Accept a staged call input into the work queue or reject it",
        },
    ],
    tables: &["call_inputs", "call_input_drive_settings"],
    env_vars: &[
        &env_registry::BOS_CALL_INPUTS_AUDIO_TRANSCRIPTION_ENABLED,
        &env_registry::BOS_CALL_INPUTS_MAX_AUDIO_BYTES,
        &env_registry::BOS_CALL_INPUTS_SYNC_ENABLED,
        &env_registry::BOS_CALL_INPUTS_SYNC_INTERVAL_SECS,
        &env_registry::BOS_CALL_INPUTS_TRANSCRIPTION_INTAKE_DIR,
        &env_registry::BOS_CALL_INPUTS_TRANSCRIPTION_MAX_CONCURRENCY,
        &env_registry::BOS_CALL_INPUTS_TRANSCRIPTION_TMP_DIR,
        &env_registry::BOS_CALL_INPUTS_TRANSCRIPTION_TIMEOUT_MS,
        &env_registry::BOS_CALL_INPUTS_WHISPER_BIN,
        &env_registry::BOS_CALL_INPUTS_WHISPER_MODEL,
    ],
    read_models: &[
        "call_inputs_status",
        "call_inputs",
        "call_input_drive_settings",
    ],
};
