export class ApiError extends Error {
  readonly status: number;
  /** Error code from the backend's `{"error": "<code>"}` payload, when present. */
  readonly code: string | null;
  /** Parsed JSON body, when the error response carried one. */
  readonly body: unknown;

  constructor(status: number, code: string | null, body: unknown) {
    super(code ?? `http_${status}`);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.body = body;
  }
}

export function isUnauthorized(err: unknown): boolean {
  return err instanceof ApiError && err.status === 401;
}

export function isRevisionConflict(err: unknown): boolean {
  return err instanceof ApiError && err.status === 409;
}

const ERROR_MESSAGES: Record<string, string> = {
  accounting_provider_not_writable: "Accounting is not set up for this action. Ask your administrator to finish setup.",
  attention_level_invalid: "Choose a valid attention filter.",
  calendar_draft_already_active: "This work item already has an active calendar draft.",
  calendar_draft_end_invalid: "Use a valid end date and time for the calendar event.",
  calendar_draft_not_found: "That calendar draft is no longer available.",
  calendar_draft_start_invalid: "Use a valid start date and time for the calendar event.",
  calendar_draft_title_required: "Add a title before saving the calendar event.",
  google_calendar_attendee_invalid: "Use one valid attendee email address per line.",
  google_calendar_attendee_limit_exceeded: "Calendar events support up to 25 attendees here.",
  google_calendar_invitation_attendees_required: "Add at least one attendee before sending calendar invitations.",
  google_calendar_scope_missing: "Reconnect Google with Calendar access before approving this live event.",
  calendar_extract_invalid_response: "AI could not turn this message into a valid calendar event. Try again.",
  calendar_extract_no_event: "AI did not find a concrete dated event in this message.",
  calendar_list_failed: "We couldn't load your calendars. Try again in a minute.",
  source_user_credential_unavailable: "The source Google account is no longer connected. Reconnect it before approving.",
  call_input_drive_folder_id_required: "Choose a Drive folder before saving call-input settings.",
  call_input_drive_interval_invalid: "Use a valid sync interval for call-input settings.",
  call_input_not_found: "That call input is no longer available.",
  call_input_not_staged: "That call input is no longer awaiting review.",
  call_input_packet_kinds_required: "Choose at least one action for accepted call inputs.",
  call_input_source_ref_required: "Choose a call source before staging.",
  call_input_status_invalid: "Choose a valid call input status.",
  call_input_summary_empty: "Add a summary before staging that call input.",
  call_input_title_empty: "Add a title before staging that call input.",
  call_input_transcript_required: "Add transcript or call-log text before staging.",
  call_source_consent_missing: "Record the call source consent basis before using it.",
  call_source_consent_not_confirmed: "That call source still needs written consent confirmation before use.",
  call_source_fit_not_confirmed: "That call source still needs technical-fit confirmation before use.",
  call_source_llm_processing_not_authorized: "That call source is not approved for AI processing yet.",
  call_source_not_configured: "That call source is not configured for this client.",
  call_source_not_enabled: "That call source is not approved and enabled.",
  claim_damage_snapshot_missing: "The damage event for this claim is no longer available.",
  claim_draft_already_active: "This work item already has an active claim draft.",
  claim_draft_amount_invalid: "Enter a valid claim amount.",
  claim_draft_amount_required: "Enter a claim amount before approving.",
  claim_draft_narrative_required: "Add a claim narrative before saving.",
  claim_draft_not_found: "That claim draft is no longer available.",
  claim_draft_to_addr_unset: "Claim emails need a recipient before they can be approved. Ask your administrator to finish setup.",
  claim_fill_invalid_response: "AI could not draft a valid claim packet. Try again.",
  claim_packet_incomplete: "Complete the required claim evidence before approving.",
  content_brief_unsearchable: "Add a more specific brief so Drive evidence can be found.",
  content_citation_gate_failed: "The draft has unsupported claims. Edit the draft or regenerate with better evidence.",
  content_draft_already_active: "This work item already has an active content draft.",
  content_draft_body_required: "Add content before saving the draft.",
  content_draft_body_too_long: "Shorten the content draft before saving.",
  content_draft_not_found: "That content draft is no longer available.",
  content_draft_title_required: "Add a title before saving the content draft.",
  content_publish_adapter_unavailable: "Direct publishing is not configured for this client.",
  content_publish_already_requested: "This draft is already queued or has been published.",
  content_publish_date_invalid: "Enter a valid publication date.",
  content_publish_meta_description_required: "Add a meta description before publishing.",
  content_publish_not_approved: "Approve the draft before publishing it.",
  content_publish_slug_invalid: "Use a lowercase URL slug with letters, numbers, and hyphens.",
  content_fill_invalid_response: "AI could not draft valid content for this item. Try again.",
  content_no_evidence: "Drive evidence is required before drafting content.",
  content_inventory_already_archived: "That published item is already archived.",
  content_inventory_canonical_key_required: "Add a title or URL before saving the published item.",
  content_inventory_not_found: "That published item is no longer available.",
  content_inventory_status_invalid: "Choose a valid published-item status.",
  content_plan_not_found: "That content plan item is no longer available.",
  content_plan_not_planned: "Only planned content items can be changed or queued.",
  content_plan_not_publishable: "Only planned or queued content items can be marked as published.",
  content_plan_status_invalid: "Choose a valid content plan status.",
  content_plan_topic_required: "Add a topic before saving the content plan item.",
  content_campaign_plan_not_queued: "Generate the campaign before publishing it.",
  content_campaign_plan_not_queueable: "Only planned or queued content can start a campaign.",
  content_campaign_plan_revision_changed: "The content plan changed — reload before generating the campaign.",
  content_campaign_plan_url_mismatch: "The published URL no longer matches this content plan.",
  content_campaign_work_item_missing: "This campaign does not have an accepted work item yet.",
  content_campaign_work_item_unavailable: "The campaign work item is no longer available.",
  content_campaign_article_not_approved: "Approve the grounded article before publishing the campaign.",
  content_campaign_article_plan_mismatch: "This article belongs to a different content plan.",
  content_campaign_article_revision_changed: "The article changed — reload and review the new revision.",
  content_campaign_article_already_approved: "This article already belongs to an approved campaign publication.",
  content_campaign_social_snapshot_required: "Generate social variants for the selected destinations.",
  content_campaign_social_snapshot_changed: "The social variants changed — reload and review the new revision.",
  content_campaign_social_snapshot_invalid: "The social variants are not grounded in this exact article and URL.",
  content_campaign_social_snapshot_already_approved: "These social variants already belong to an approved campaign.",
  content_campaign_destination_set_invalid: "Choose only configured social destinations.",
  content_campaign_preview_url_slug_mismatch: "The blog slug must match the expected canonical URL.",
  content_campaign_blog_job_missing: "The campaign's blog delivery job is missing. Ask an administrator to inspect the audit trail.",
  content_campaign_social_job_missing: "A campaign social delivery job is missing. Ask an administrator to inspect the audit trail.",
  content_campaign_publication_state_changed: "The campaign publication changed — reload before trying again.",
  content_campaign_snapshot_invalid: "The approved campaign snapshot could not be verified. No dependent posts were created.",
  content_publish_owned_by_campaign: "This article is owned by an approved campaign. Use the Content workspace.",
  published_url_required: "Add the published URL before marking the content item as published.",
  social_canonical_url_invalid: "Enter the canonical published HTTPS URL.",
  social_draft_confidence_invalid: "AI returned an invalid confidence value. Try generating again.",
  social_draft_grounding_invalid: "AI copy was not grounded in the published source. Review the source and try again.",
  social_draft_grounding_missing: "AI copy did not cite the published source. Try generating again.",
  social_draft_output_invalid: "AI could not draft valid social copy. Try again.",
  social_draft_target_set_invalid: "AI did not return exactly one draft for every configured channel.",
  social_external_id_invalid: "Add a valid source-system content identifier.",
  social_generation_capacity_exceeded: "Social drafting is busy. Try again shortly.",
  social_generation_already_running: "Social drafting is already running for this article.",
  social_generation_run_changed: "The article changed during drafting. Reload and generate again.",
  social_generation_spawn_failed: "Social drafting could not start. Try again.",
  social_published_at_invalid: "Use a valid publication timestamp.",
  social_approval_snapshot_invalid: "That approval snapshot changed unexpectedly. Reload and try again.",
  social_channel_job_set_invalid: "The channel delivery set is incomplete. Reload and try again.",
  social_channel_job_target_mismatch: "The channel delivery no longer matches this proposal. Reload and try again.",
  social_channel_set_invalid: "The proposal contains an unknown or duplicate Buffer channel.",
  social_channel_configuration_changed: "Buffer channels changed — create a fresh proposal.",
  social_channel_set_incomplete: "Add copy for every configured Buffer channel.",
  social_channels_config_invalid: "Buffer channel configuration is invalid.",
  social_channels_not_configured: "Configure Buffer channels before staging social posts.",
  social_image_url_invalid: "Use a public HTTPS image URL.",
  buffer_instagram_image_required: "Add an image before approving an Instagram post.",
  social_post_text_required: "Add post text for every channel.",
  social_post_text_too_long: "Shorten the post text for this channel.",
  social_proposal_already_exists: "A proposal already exists for this request. Reload the list.",
  social_proposal_not_found: "That social proposal is no longer available.",
  social_proposal_not_staged: "Only staged proposals can be edited or decided.",
  social_proposal_campaign_locked: "This proposal is locked to an approved campaign revision.",
  social_preview_requires_campaign_approval: "Pre-publication social variants must be approved from the Content campaign workspace.",
  social_preview_article_unavailable: "The article for these social variants is no longer available.",
  social_preview_article_revision_changed: "The article changed after these social variants were grounded. Generate them again.",
  social_published_source_not_found: "That published blog post is no longer available.",
  social_published_source_changed: "That published blog post changed elsewhere. Reload and try again.",
  social_published_source_identity_changed: "This source identifier already belongs to a different canonical blog post.",
  social_published_source_not_live: "The selected blog post does not have a live published URL yet.",
  social_published_source_url_mismatch: "Use the selected blog post's canonical published URL.",
  social_queue_due_at_invalid: "Queue mode cannot include a scheduled time.",
  social_schedule_due_at_invalid: "Enter a valid scheduled date and time.",
  social_schedule_due_at_required: "Choose a scheduled date and time.",
  social_source_excerpt_too_long: "Shorten the published article excerpt.",
  social_source_already_has_proposal: "This article already has a staged social proposal.",
  social_source_generation_state_invalid: "This article cannot be drafted from its current state. Reload and try again.",
  social_source_kind_invalid: "Use a valid publishing source name.",
  social_source_title_invalid: "Add a valid published article title.",
  social_utm_parameters_incomplete: "UTM source, medium, and campaign must be set together.",
  social_utm_source_too_long: "Shorten the UTM source value.",
  social_utm_medium_too_long: "Shorten the UTM medium value.",
  social_utm_campaign_too_long: "Shorten the UTM campaign value.",
  social_utm_content_too_long: "Shorten the UTM content value.",
  crm_draft_already_active: "This work item already has an active CRM note draft.",
  crm_draft_body_required: "Add CRM note text before saving.",
  crm_draft_contact_email_invalid: "Use a valid CRM contact email.",
  crm_draft_not_found: "That CRM note draft is no longer available.",
  crm_cache_admin_only: "Only an administrator can start a CRM sync.",
  crm_cache_context_requires_source_key: "Open a message before loading CRM context.",
  crm_cache_contact_lookup_requires_one_key: "Search CRM contacts by one email or company at a time.",
  crm_cache_deal_lookup_requires_contact_email: "Search CRM deals by a contact email.",
  crm_fill_invalid_response: "AI could not draft a valid CRM note. Try again.",
  crm_note_records_first: "Create the CRM record for this contact before approving the note.",
  crm_provider_invalid: "CRM is not set up correctly. Ask your administrator to finish setup.",
  crm_record_draft_already_active: "This work item already has an active CRM records draft.",
  crm_record_fill_invalid_response: "AI could not draft valid CRM records. Try again.",
  crm_record_draft_not_found: "That CRM records draft is no longer available.",
  crm_record_draft_not_staged: "That CRM records draft is no longer awaiting review.",
  crm_record_contact_last_name_required: "Add a contact last name before creating this EspoCRM record.",
  crm_sales_intent_already_active: "This work item already has an active CRM lead draft.",
  crm_sales_intent_contact_email_invalid: "Use a valid CRM lead contact email.",
  crm_sales_intent_not_found: "That CRM lead draft is no longer available.",
  crm_sales_intent_not_staged: "That CRM lead draft is no longer awaiting review.",
  crm_sales_intent_follow_up_due_date_invalid: "Use a follow-up date in YYYY-MM-DD format.",
  crm_sales_intent_invalid_response: "AI could not draft a valid sales-intent record. Try again.",
  crm_sales_intent_next_step_required: "Add a next step before saving the CRM lead draft.",
  crm_sales_intent_provider_unsupported: "This CRM provider does not support lead creation here yet.",
  crm_sales_intent_summary_required: "Add a summary before saving the CRM lead draft.",
  crm_sales_intent_target_unsupported: "This sales-intent target is not supported for approval yet.",
  crm_sales_intent_title_required: "Add a title before saving the CRM lead draft.",
  customer_tier_sync_no_actions: "There are no mapped customer tiers to sync.",
  customer_tier_sync_run_not_found: "That customer tier sync run is no longer available.",
  customer_tier_sync_run_not_staged: "That customer tier sync run is no longer awaiting review.",
  domain_seed_invalid: "Enter a valid website domain and try again.",
  drive_corpus_folder_id_required: "Choose at least one Drive folder before saving corpus settings.",
  drive_search_query_empty: "Enter a search term before searching Drive.",
  email_draft_already_active: "This work item already has an active email draft.",
  email_draft_body_required: "Add email body text before saving.",
  email_draft_cc_addrs_invalid: "Use valid Cc email addresses before saving.",
  email_draft_confidence_invalid: "AI returned an invalid email draft. Try rewriting again.",
  email_draft_not_found: "That email draft is no longer available.",
  email_draft_not_staged: "That email draft is no longer awaiting review.",
  email_draft_subject_invalid: "Remove line breaks or control characters from the email subject.",
  email_draft_subject_required: "Add an email subject before saving.",
  email_draft_to_addr_invalid: "Use valid email recipients before saving.",
  email_draft_to_addr_required: "Add at least one email recipient before saving.",
  email_fill_invalid_response: "AI could not draft a valid email reply. Try again.",
  email_rewrite_failed: "AI could not rewrite this email. Try again.",
  email_follow_up_due_date_invalid: "Use a valid follow-up due date.",
  email_follow_up_due_date_required: "Choose a follow-up due date.",
  email_follow_up_not_due: "That follow-up is not due yet.",
  email_follow_up_not_found: "That email follow-up is no longer available.",
  email_follow_up_not_open: "That email follow-up is no longer open.",
  email_follow_up_not_waiting_reply: "Check the thread before drafting a follow-up reply.",
  email_follow_up_status_invalid: "Choose a valid follow-up status.",
  email_follow_up_title_required: "Add a follow-up title.",
  email_attachment_fetch_failed: "We couldn't fetch that attachment from Gmail. Try again in a minute.",
  email_attachment_not_found: "That email attachment changed or is no longer available. Reload and select it again.",
  email_attachment_too_large: "That attachment is too large to stage for an agent session.",
  email_body_compaction_batch_empty: "No email bodies were selected for this maintenance batch.",
  email_inbound_message_not_found: "That email is no longer available.",
  email_triage_category_id_invalid: "Use only letters, numbers, dashes, and underscores in the category id.",
  email_triage_category_in_use: "This category is still used by rules or work items.",
  email_triage_category_is_system: "System categories cannot be changed here.",
  email_triage_category_name_required: "Add a category name.",
  email_triage_category_not_found: "That category is no longer available.",
  email_triage_category_policy_mismatch: "The category and its work-item settings no longer match. Reopen the form and try again.",
  email_triage_category_unknown: "Choose a known category.",
  email_triage_condition_value_required: "Add a value for each rule condition that needs one.",
  email_triage_crm_deal_facet_invalid: "Choose a valid CRM deal filter.",
  email_triage_header_name_not_allowed: "Only header conditions can include a header name.",
  email_triage_header_name_required: "Choose a header name for each header condition.",
  email_triage_rule_conditions_required: "Add at least one condition before saving the rule.",
  email_triage_rule_id_required: "Add a rule id before saving the rule.",
  email_triage_rule_not_found: "That rule is no longer available.",
  email_triage_rule_set_schema_version_invalid: "This rule format is not supported. Reload and try again.",
  email_triage_visible_gmail_categories_empty: "Keep at least one Gmail tab visible.",
  email_job_build_failed: "We couldn't prepare that email. Try again in a minute.",
  expected_revision_conflict: "This item changed elsewhere. Reload Queue and try again.",
  expected_revision_required: "This item changed elsewhere. Reload Queue and try again.",
  agent_evidence_stage_join_failed: "We couldn't stage that attachment. Try again in a minute.",
  agent_evidence_write_failed: "We couldn't write the staged attachment file. Ask your administrator to check the evidence directory.",
  agent_session_id_required: "Choose an agent session before staging an attachment.",
  gmail_credential_missing: "Reconnect Gmail before staging that attachment.",
  google_credential_not_connected: "Reconnect Google before using this action.",
  google_analytics_sync_spawn_failed: "We couldn't start the GA4 sync. Try again in a minute.",
  google_drive_scope_missing: "Reconnect Google with Drive access before using this action.",
  drive_corpus_folder_env_pinned: "Drive corpus folders are managed by deployment config.",
  enrichment_run_not_found: "That enrichment run is no longer available.",
  enrichment_source_missing: "The source for this draft is no longer available.",
  follow_up_draft_already_active: "This work item already has an active follow-up draft.",
  follow_up_draft_due_date_invalid: "Use a valid follow-up due date.",
  follow_up_item_missing: "We couldn't open that follow-up work item. Try again in a minute.",
  follow_up_draft_not_found: "That follow-up draft is no longer available.",
  follow_up_draft_title_required: "Add a follow-up title.",
  follow_up_fill_invalid_response: "AI could not draft a valid follow-up task. Try again.",
  grounding_evidence_row_limit: "Select fewer evidence rows and try again.",
  hubspot_closed_date_property_required: "Choose the HubSpot closed-date property before saving.",
  hubspot_pipeline_id_required: "Choose a HubSpot pipeline before saving.",
  hubspot_stage_id_required: "Choose a HubSpot stage before saving.",
  hubspot_stage_mapping_duplicate: "Each HubSpot stage can only be mapped once.",
  hubspot_stage_mapping_incomplete: "Complete every HubSpot stage mapping before saving.",
  hubspot_started_date_property_required: "Choose the HubSpot started-date property before saving.",
  idempotency_key_required: "Please try again. The request was missing a safety check.",
  invoice_default_due_days_out_of_range: "Use a default invoice due window between 0 and 365 days.",
  invoice_draft_already_active: "This work item already has an active invoice draft.",
  invoice_draft_customer_required: "Add a customer before saving the invoice draft.",
  invoice_draft_date_invalid: "Use a valid invoice date.",
  invoice_draft_email_invalid: "Use a valid customer email.",
  invoice_draft_email_required: "Add a customer email before approving.",
  invoice_draft_line_items_invalid: "Fix the invoice line items before saving.",
  invoice_draft_not_found: "That invoice draft is no longer available.",
  invoice_draft_not_staged: "That invoice draft is no longer awaiting review.",
  invoice_draft_total_required: "Add at least one invoice line item with an amount.",
  invoice_fill_invalid_response: "AI could not draft a valid invoice. Try again.",
  ledger_draft_already_active: "This work item already has an active ledger draft.",
  ledger_draft_amount_invalid: "Enter a valid payment amount.",
  ledger_draft_date_invalid: "Use a valid payment date.",
  ledger_draft_not_found: "That ledger draft is no longer available.",
  ledger_draft_payer_email_invalid: "Use a valid payer email.",
  ledger_draft_payer_required: "Add a payer before saving the ledger draft.",
  ledger_fill_invalid_response: "AI could not draft a valid ledger entry. Try again.",
  lead_finding_evidence_required: "Add evidence before staging that lead finding.",
  lead_finding_not_found: "That lead finding is no longer available.",
  lead_finding_not_staged: "That lead finding is no longer awaiting review.",
  lead_finding_status_invalid: "Choose a valid lead finding status.",
  lead_finding_summary_empty: "Add a summary before staging that lead finding.",
  lead_finding_title_empty: "Add a title before staging that lead finding.",
  lead_source_not_configured: "That lead source is not configured for this client.",
  lead_source_not_enabled: "That lead source is not approved and enabled.",
  llm_api_not_configured: "AI is not connected yet. Ask your administrator to finish setup.",
  llm_backend_invalid: "Choose a valid AI backend.",
  llm_harness_model_not_configured: "AI drafting is not ready yet. Ask your administrator to finish setup.",
  llm_harness_unavailable: "Claude subscription mode is not enabled on this instance.",
  llm_auth_action_invalid: "BusinessOS rejected an unsupported Claude authorization action.",
  llm_subscription_auth_in_progress: "Another operator is already connecting the Claude subscription.",
  llm_subscription_auth_flow_not_found: "That Claude sign-in expired. Start a fresh connection.",
  llm_subscription_auth_code_already_submitted: "That authorization code was already submitted. Check the connection status.",
  llm_subscription_authorization_code_invalid: "Paste the complete one-time authorization code from Claude.",
  llm_subscription_auth_start_timeout: "Claude did not start the sign-in flow in time. Try again.",
  llm_subscription_auth_start_failed: "Claude could not start the sign-in flow.",
  llm_subscription_auth_submit_failed: "Claude could not accept the authorization code. Start a fresh connection.",
  llm_subscription_status_failed: "BusinessOS could not check the Claude subscription status.",
  revision_conflict: "This setting changed elsewhere. Reload and try again.",
  llm_max_tokens_invalid: "Use a valid AI token limit.",
  llm_output_caps_exceeded: "The AI response was too large. Try again with a shorter item.",
  llm_output_missing_field: "The AI response was incomplete. Try again.",
  llm_output_not_object: "The AI response could not be read. Try again.",
  llm_output_redaction_failed: "The AI response included sensitive-looking text, so it was blocked.",
  llm_output_schema_unregistered: "AI drafting is not ready yet. Ask your administrator to finish setup.",
  llm_purpose_required: "Choose an AI task before saving settings.",
  llm_purpose_unknown: "Choose a known AI task before saving settings.",
  llm_timeout_ms_invalid: "Use a valid AI timeout.",
  message_id_required: "Choose an email and try again.",
  source_key_required: "Choose an email and try again.",
  nothing_to_enrich: "There is nothing new to fill in for this draft.",
  oauth_app_unconfigured: "This connection is not set up yet. Ask your administrator to finish setup.",
  oauth_callback_missing_params: "The sign-in response was incomplete. Try reconnecting.",
  oauth_code_exchange_failed: "The connection could not be completed. Try reconnecting.",
  oauth_state_invalid: "The connection expired. Start again from this browser.",
  oauth_task_failed: "The connection could not be saved. Try again in a minute.",
  operator_note_body_empty: "Add a note before saving.",
  operator_token_invalid: "That token did not start a session.",
  operator_user_already_archived: "That user is already archived.",
  operator_user_archived: "That user is archived.",
  operator_user_archive_requires_disabled: "Disable the user before archiving them.",
  operator_user_exists: "That operator already exists.",
  operator_user_has_google_credentials: "Disconnect that user's Google account before archiving them.",
  operator_user_has_qbo_credential: "Disconnect and purge QBO before archiving the user who connected it.",
  operator_user_not_found: "That operator is no longer available.",
  operator_user_self_archive: "You cannot archive the user you are currently signed in as.",
  owner_report_bad_period_end: "Use a valid report end date.",
  owner_report_bad_period_start: "Use a valid report start date.",
  owner_report_not_found: "That report is no longer available.",
  owner_report_period_invalid: "Choose a valid report period.",
  owner_report_scope_forbidden: "Owner reports are only available to the configured owner.",
  owner_report_to_addr_unset: "Owner report emails need a recipient before they can be sent. Ask your administrator to finish setup.",
  packet_proposal_category_invalid: "This email's category is no longer available. Reload and try again.",
  packet_proposal_join_failed: "Smart draft could not finish. Try again in a minute.",
  packet_proposal_kind_not_enabled: "Smart draft is not enabled for that action.",
  packet_proposal_no_candidates: "Smart draft is not available for this email's category yet.",
  packet_proposal_output_invalid: "The AI response could not be turned into drafts. Try again.",
  packet_proposal_run_not_found: "Smart draft could not find its saved work. Reload and try again.",
  packet_proposal_source_not_found: "That email is no longer available.",
  packet_proposal_source_required: "Choose an email and try again.",
  packet_proposal_source_unsupported: "Smart draft is not available for this source yet.",
  email_trash_expected_revision_without_work_item:
    "This email has no Queue revision to compare. Reload Inbox and try again.",
  email_trash_source_unsupported: "Only source emails can be moved to Gmail Trash.",
  qbo_financial_scope_forbidden: "QuickBooks financials are only visible to the operator who connected QuickBooks.",
  produce_item_not_accepted: "Accept the work item before producing a draft.",
  produce_kind_not_suggested: "Add this action to the work item before producing its draft.",
  produce_source_missing: "The source item is no longer available.",
  produce_status_query_required: "Choose a work item before checking draft status.",
  produce_source_unsupported: "That work item's source type cannot be used for this action.",
  quote_draft_not_found: "That quote draft is no longer available.",
  quote_draft_not_staged: "That quote draft is no longer awaiting review.",
  quote_guardrail_approval_required: "This quote needs the required approval before it can continue.",
  quote_guardrail_delta_overflow: "The quote price change is too large to evaluate.",
  quote_guardrail_price_sku_required: "Choose a SKU before checking quote price rules.",
  quote_guardrail_price_unit_cents_invalid: "Enter a valid quoted unit price.",
  quote_line_amount_invalid: "Enter a valid quote line amount.",
  quote_line_description_required: "Add a quote line description.",
  quote_line_item_required: "Add at least one quote line item.",
  quote_line_not_grounded: "Every quote line needs supporting evidence.",
  quote_line_quantity_invalid: "Enter a valid quote line quantity.",
  quote_line_sku_required: "Choose a SKU for each quote line.",
  quote_line_total_mismatch: "Fix the quote line total before saving.",
  quote_line_total_overflow: "The quote line total is too large.",
  quote_profile_stage_step_missing: "Add a quote stage step before saving the profile.",
  quote_profile_stage_step_must_be_last: "The quote stage step must be the final workflow step.",
  quote_profile_step_node_required: "Choose a workflow node for each quote profile step.",
  quote_summary_required: "Add a quote summary.",
  quote_workflow_busy: "That quote workflow is already running. Try again after it finishes.",
  quote_workflow_not_found: "That quote workflow is no longer available.",
  quote_workflow_run_missing: "That quote workflow run is no longer available.",
  quote_workflow_start_receipt_missing: "The quote workflow start receipt is missing. Try again.",
  receipt_payload_compaction_batch_empty: "No receipt payloads were selected for this maintenance batch.",
  release_note_id_required: "Choose an update before continuing.",
  release_note_not_found: "That update is no longer available.",
  release_note_summary_empty: "Add release note text before publishing.",
  runtime_setting_invalid_value: "Enter a valid value for that runtime setting.",
  runtime_setting_not_editable: "That setting can only be changed in deployment config.",
  runtime_setting_unknown_var: "That setting is no longer available.",
  runtime_setting_value_required: "Enter a value before saving that setting.",
  search_console_config_overrides_selection: "Search Console property selection is managed by deployment config.",
  search_console_property_not_discovered: "Choose a discovered Search Console property.",
  search_console_sync_spawn_failed: "We couldn't start the Search Console sync. Try again in a minute.",
  research_mode_unavailable: "Research enrichment is not available for this draft.",
  research_domain_missing: "Enter the company's website domain before running Research.",
  research_mode_disabled: "Research mode is not enabled for this workspace.",
  research_concurrency_limit: "Another research run is already in progress. Try again after it finishes.",
  route_not_found: "That page or action is not available.",
  session_generation_failed: "We couldn't start a secure browser session. Try again in a minute.",
  shopify_tier_mapping_empty: "Shopify tier sync needs at least one configured QBO tier mapping.",
  shopify_tier_mapping_invalid: "Shopify tier sync mapping is not valid JSON. Ask your administrator to fix setup.",
  shopify_tier_mapping_target_missing: "Each Shopify tier mapping needs a tag or metafield target.",
  shopify_tier_mapping_unconfigured: "Shopify tier sync is not configured yet. Ask your administrator to add tier mappings.",
  storage_failure: "We couldn't save your change. Try again in a minute.",
  task_not_found: "That task is no longer available.",
  task_status_invalid: "Choose a valid task status.",
  task_today_invalid: "Choose a valid date.",
  tier_sync_stage_missing: "We couldn't prepare that tier sync run. Try again in a minute.",
  token_generation_failed: "We couldn't create that access token. Try again.",
  work_item_not_found: "That work item is no longer available.",
  work_item_source_missing: "The source for that work item is no longer available.",
  work_item_guidance_not_editable: "Guidance can only be edited while the item is open.",
  work_item_guidance_too_long: "Shorten the work item guidance before saving.",
  work_item_kinds_not_editable: "Actions can only be edited while the item is open.",
  work_queue_ai_gmail_scope_fallback_only: "AI Gmail scope cannot be set to fallback only.",
  work_queue_ai_gmail_scope_selected_empty: "Choose at least one Gmail account for AI triage.",
  work_queue_ai_suggest_all_exclusive: "AI suggest-all cannot be combined with selected actions.",
  work_queue_assignee_not_active: "That operator is disabled or archived.",
  work_queue_assignee_not_visible: "That operator cannot see this item.",
  work_queue_assignment_named_user_required: "Sign in with a personal operator account to assign work to yourself.",
  work_queue_assignee_required: "Choose an operator to assign this item.",
  work_queue_category_required: "Choose a work queue category.",
  work_queue_unassign_forbidden: "Only the current assignee can unassign this item.",
  work_queue_status_invalid: "Choose a valid work queue view.",
};

export function friendlyErrorLabel(code: string | null | undefined): string {
  return code ? "Failed" : "AI call failed";
}

export function friendlyDiagnosticErrorLabel(
  code: string | null | undefined,
  context?: {
    source?: string | null;
    category?: string | null;
  },
): string {
  if (context?.source === "outbox" || context?.category === "provider_delivery") {
    return "Provider delivery failed";
  }
  if (!code) return "Failed";
  if (code === "typed_llm_harness_session_exited") return "AI run stopped";
  if (code.startsWith("llm_api_") || code.startsWith("llm_harness_")) {
    return "AI setup needed";
  }
  if (code.startsWith("typed_llm_harness_")) return "AI run failed";
  if (code.startsWith("llm_output_")) return "AI response blocked";
  if (code.includes("provider") || code.includes("outbox")) {
    return "Provider delivery failed";
  }
  if (code.includes("storage") || code.includes("write_failed")) {
    return "Storage error";
  }
  if (code.includes("not_found")) return "Missing record";
  if (code === "panic") return "System error";
  return "Failed";
}

function apiErrorDetail(err: ApiError): string | null {
  if (err.body === null || typeof err.body !== "object") return null;
  const message = (err.body as { message?: unknown }).message;
  if (typeof message !== "string") return null;
  const trimmed = message.trim();
  return trimmed.length > 0 ? trimmed : null;
}

export function errorCodeMessage(code: string | null | undefined): string | null {
  return code ? ERROR_MESSAGES[code] ?? null : null;
}

export function errorMessage(err: unknown): string {
  if (err instanceof ApiError) {
    if (err.code) console.debug("[bos] api error", err.code, err.status);
    const detail = apiErrorDetail(err);
    const codeMessage = errorCodeMessage(err.code);
    if (codeMessage) {
      const message = codeMessage;
      return detail ? `${message} Reason: ${detail}` : message;
    }
    if (err.status === 401) return "Your access token is missing or expired.";
    if (err.status === 409) return "This changed elsewhere. Reload and try again.";
    if (err.status === 404) return "That item is no longer available.";
    return "Something went wrong — please try again.";
  }
  if (err instanceof Error) return err.message;
  return "Something went wrong — please try again.";
}

export async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const headers = new Headers(init?.headers);
  if (init?.body != null) headers.set("Content-Type", "application/json");

  const res = await fetch(path, { ...init, credentials: "same-origin", headers });

  let body: unknown = null;
  const text = await res.text();
  if (text.length > 0) {
    try {
      body = JSON.parse(text);
    } catch {
      body = text;
    }
  }

  if (!res.ok) {
    const code =
      body !== null &&
      typeof body === "object" &&
      "error" in body &&
      typeof (body as { error: unknown }).error === "string"
        ? (body as { error: string }).error
        : null;
    throw new ApiError(res.status, code, body);
  }

  return body as T;
}
