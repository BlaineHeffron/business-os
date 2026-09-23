import type { AiRetriageResetRequest } from "../../types/generated/AiRetriageResetRequest";
import type { AiRetriageResetResponse } from "../../types/generated/AiRetriageResetResponse";
import type { AiUsageResponse } from "../../types/generated/AiUsageResponse";
import type { AdminSettingClearRequest } from "../../types/generated/AdminSettingClearRequest";
import type { AdminSettingUpdateRequest } from "../../types/generated/AdminSettingUpdateRequest";
import type { AdminSettingsResponse } from "../../types/generated/AdminSettingsResponse";
import type { AttentionLevel } from "../../types/generated/AttentionLevel";
import type { DebugDiagnosticsResponse } from "../../types/generated/DebugDiagnosticsResponse";
import type { DebugSpawnAgentRequest } from "../../types/generated/DebugSpawnAgentRequest";
import type { DebugSpawnAgentResponse } from "../../types/generated/DebugSpawnAgentResponse";
import type { InstanceHealth } from "../../types/generated/InstanceHealth";
import type { LaunchAgentRequest } from "../../types/generated/LaunchAgentRequest";
import type { LaunchAgentResponse } from "../../types/generated/LaunchAgentResponse";
import type { CalendarDraftActionRequest } from "../../types/generated/CalendarDraftActionRequest";
import type { CalendarDraftUpdateRequest } from "../../types/generated/CalendarDraftUpdateRequest";
import type { CalendarDraftProduceRequest } from "../../types/generated/CalendarDraftProduceRequest";
import type { CalendarDraftProduceResponse } from "../../types/generated/CalendarDraftProduceResponse";
import type { CalendarDraftsResponse } from "../../types/generated/CalendarDraftsResponse";
import type { CalendarListResponse } from "../../types/generated/CalendarListResponse";
import type { CallInputActionRequest } from "../../types/generated/CallInputActionRequest";
import type { CallInputsDriveSettingsResponse } from "../../types/generated/CallInputsDriveSettingsResponse";
import type { CallInputsDriveSettingsUpdateRequest } from "../../types/generated/CallInputsDriveSettingsUpdateRequest";
import type { CallInputsResponse } from "../../types/generated/CallInputsResponse";
import type { CallInputsStatusResponse } from "../../types/generated/CallInputsStatusResponse";
import type { CategoriesListResponse } from "../../types/generated/CategoriesListResponse";
import type { CategoryDeleteRequest } from "../../types/generated/CategoryDeleteRequest";
import type { CategoryUpsertRequest } from "../../types/generated/CategoryUpsertRequest";
import type { ClaimDraftActionRequest } from "../../types/generated/ClaimDraftActionRequest";
import type { ClaimDraftProduceRequest } from "../../types/generated/ClaimDraftProduceRequest";
import type { ClaimDraftProduceResponse } from "../../types/generated/ClaimDraftProduceResponse";
import type { ClaimDraftUpdateRequest } from "../../types/generated/ClaimDraftUpdateRequest";
import type { ClaimDraftsResponse } from "../../types/generated/ClaimDraftsResponse";
import type { ConnectorStatus } from "../../types/generated/ConnectorStatus";
import type { GoogleDriveFolderOptionsResponse } from "../../types/generated/GoogleDriveFolderOptionsResponse";
import type { ContentDraftActionRequest } from "../../types/generated/ContentDraftActionRequest";
import type { ContentDraftProduceRequest } from "../../types/generated/ContentDraftProduceRequest";
import type { ContentDraftProduceResponse } from "../../types/generated/ContentDraftProduceResponse";
import type { ContentDraftPublishRequest } from "../../types/generated/ContentDraftPublishRequest";
import type { ContentDraftUpdateRequest } from "../../types/generated/ContentDraftUpdateRequest";
import type { ContentDraftsResponse } from "../../types/generated/ContentDraftsResponse";
import type { ContentDraftOverlapResponse } from "../../types/generated/ContentDraftOverlapResponse";
import type { ContentInventoryArchiveRequest } from "../../types/generated/ContentInventoryArchiveRequest";
import type { ContentInventoryManualAddRequest } from "../../types/generated/ContentInventoryManualAddRequest";
import type { ContentInventoryRefreshRequest } from "../../types/generated/ContentInventoryRefreshRequest";
import type { ContentInventoryResponse } from "../../types/generated/ContentInventoryResponse";
import type { ContentInventoryStatus } from "../../types/generated/ContentInventoryStatus";
import type { ContentPlanItemCheckRequest } from "../../types/generated/ContentPlanItemCheckRequest";
import type { ContentPlanItemCreateRequest } from "../../types/generated/ContentPlanItemCreateRequest";
import type { ContentPlanItemMarkPublishedRequest } from "../../types/generated/ContentPlanItemMarkPublishedRequest";
import type { ContentPlanItemQueueRequest } from "../../types/generated/ContentPlanItemQueueRequest";
import type { ContentPlanItemUpdateRequest } from "../../types/generated/ContentPlanItemUpdateRequest";
import type { ContentPlanItemsResponse } from "../../types/generated/ContentPlanItemsResponse";
import type { ContentPlanStatus } from "../../types/generated/ContentPlanStatus";
import type { ContentCampaignGenerateRequest } from "../../types/generated/ContentCampaignGenerateRequest";
import type { ContentCampaignPublishRequest } from "../../types/generated/ContentCampaignPublishRequest";
import type { ContentCampaignWorkspaceResponse } from "../../types/generated/ContentCampaignWorkspaceResponse";
import type { CrmDraftActionRequest } from "../../types/generated/CrmDraftActionRequest";
import type { DriveCorpusStatus } from "../../types/generated/DriveCorpusStatus";
import type { DriveCorpusSettingsUpdateRequest } from "../../types/generated/DriveCorpusSettingsUpdateRequest";
import type { DriveCorpusSettingsUpdateResponse } from "../../types/generated/DriveCorpusSettingsUpdateResponse";
import type { DriveSearchResponse } from "../../types/generated/DriveSearchResponse";
import type { DriveSyncNowResponse } from "../../types/generated/DriveSyncNowResponse";
import type { SearchConsolePropertySelectRequest } from "../../types/generated/SearchConsolePropertySelectRequest";
import type { SearchConsoleSyncNowResponse } from "../../types/generated/SearchConsoleSyncNowResponse";
import type { SearchConsoleTrafficOverview } from "../../types/generated/SearchConsoleTrafficOverview";
import type { SocialProposalActionRequest } from "../../types/generated/SocialProposalActionRequest";
import type { SocialGenerationResponse } from "../../types/generated/SocialGenerationResponse";
import type { SocialProposalGenerateRequest } from "../../types/generated/SocialProposalGenerateRequest";
import type { SocialProposalStageRequest } from "../../types/generated/SocialProposalStageRequest";
import type { SocialProposalUpdateRequest } from "../../types/generated/SocialProposalUpdateRequest";
import type { SocialPublishingResponse } from "../../types/generated/SocialPublishingResponse";
import type { SocialDraftPreviewGenerateRequest } from "../../types/generated/SocialDraftPreviewGenerateRequest";
import type { InventoryAlertsResponse } from "../../types/generated/InventoryAlertsResponse";
import type { InvoiceDraftActionRequest } from "../../types/generated/InvoiceDraftActionRequest";
import type { InvoiceDraftProduceRequest } from "../../types/generated/InvoiceDraftProduceRequest";
import type { InvoiceDraftProduceResponse } from "../../types/generated/InvoiceDraftProduceResponse";
import type { InvoiceDraftUpdateRequest } from "../../types/generated/InvoiceDraftUpdateRequest";
import type { InvoiceDraftsResponse } from "../../types/generated/InvoiceDraftsResponse";
import type { InvoiceSettingsResponse } from "../../types/generated/InvoiceSettingsResponse";
import type { InvoiceSettingsUpdateRequest } from "../../types/generated/InvoiceSettingsUpdateRequest";
import type { InventoryOrdersResponse } from "../../types/generated/InventoryOrdersResponse";
import type { InventoryPurchaseOrdersResponse } from "../../types/generated/InventoryPurchaseOrdersResponse";
import type { InventoryStockResponse } from "../../types/generated/InventoryStockResponse";
import type { InventorySyncNowResponse } from "../../types/generated/InventorySyncNowResponse";
import type { StockforgeConnectorStatus } from "../../types/generated/StockforgeConnectorStatus";
import type { LedgerDraftActionRequest } from "../../types/generated/LedgerDraftActionRequest";
import type { LedgerDraftProduceRequest } from "../../types/generated/LedgerDraftProduceRequest";
import type { LedgerDraftProduceResponse } from "../../types/generated/LedgerDraftProduceResponse";
import type { LedgerDraftUpdateRequest } from "../../types/generated/LedgerDraftUpdateRequest";
import type { LedgerDraftsResponse } from "../../types/generated/LedgerDraftsResponse";
import type { LeadDiscoveryStatusResponse } from "../../types/generated/LeadDiscoveryStatusResponse";
import type { LeadFindingActionRequest } from "../../types/generated/LeadFindingActionRequest";
import type { LeadFindingStageRequest } from "../../types/generated/LeadFindingStageRequest";
import type { LeadFindingsResponse } from "../../types/generated/LeadFindingsResponse";
import type { LlmRouteSettingsResponse } from "../../types/generated/LlmRouteSettingsResponse";
import type { LlmRouteSettingsUpdateRequest } from "../../types/generated/LlmRouteSettingsUpdateRequest";
import type { ClaudeSubscriptionAuthCompleteRequest } from "../../types/generated/ClaudeSubscriptionAuthCompleteRequest";
import type { ClaudeSubscriptionAuthStartRequest } from "../../types/generated/ClaudeSubscriptionAuthStartRequest";
import type { ClaudeSubscriptionAuthStartResponse } from "../../types/generated/ClaudeSubscriptionAuthStartResponse";
import type { ClaudeSubscriptionStatus } from "../../types/generated/ClaudeSubscriptionStatus";
import type { OperatorUserDefaultCalendarRequest } from "../../types/generated/OperatorUserDefaultCalendarRequest";
import type { AccountingAgingResponse } from "../../types/generated/AccountingAgingResponse";
import type { AccountingConnectorStatus } from "../../types/generated/AccountingConnectorStatus";
import type { AccountingCustomersResponse } from "../../types/generated/AccountingCustomersResponse";
import type { AccountingInvoicesResponse } from "../../types/generated/AccountingInvoicesResponse";
import type { AccountingFinancialsResponse } from "../../types/generated/AccountingFinancialsResponse";
import type { AccountingSyncNowResponse } from "../../types/generated/AccountingSyncNowResponse";
import type { CrmCacheSyncInfo } from "../../types/generated/CrmCacheSyncInfo";
import type { CrmCacheSyncNowResponse } from "../../types/generated/CrmCacheSyncNowResponse";
import type { CrmContextResponse } from "../../types/generated/CrmContextResponse";
import type { CrmContactSnapshotsResponse } from "../../types/generated/CrmContactSnapshotsResponse";
import type { CrmDealSnapshotsResponse } from "../../types/generated/CrmDealSnapshotsResponse";
import type { CustomerTierSyncApproveRequest } from "../../types/generated/CustomerTierSyncApproveRequest";
import type { CustomerTierSyncListResponse } from "../../types/generated/CustomerTierSyncListResponse";
import type { CustomerTierSyncPreviewRequest } from "../../types/generated/CustomerTierSyncPreviewRequest";
import type { CustomerTierSyncRun } from "../../types/generated/CustomerTierSyncRun";
import type { CrmDraftUpdateRequest } from "../../types/generated/CrmDraftUpdateRequest";
import type { CrmDraftProduceRequest } from "../../types/generated/CrmDraftProduceRequest";
import type { CrmDraftProduceResponse } from "../../types/generated/CrmDraftProduceResponse";
import type { CrmDraftsResponse } from "../../types/generated/CrmDraftsResponse";
import type { CrmRecordDraftActionRequest } from "../../types/generated/CrmRecordDraftActionRequest";
import type { CrmRecordDraftUpdateRequest } from "../../types/generated/CrmRecordDraftUpdateRequest";
import type { CrmRecordDraftProduceRequest } from "../../types/generated/CrmRecordDraftProduceRequest";
import type { CrmRecordDraftProduceResponse } from "../../types/generated/CrmRecordDraftProduceResponse";
import type { CrmRecordDraftsResponse } from "../../types/generated/CrmRecordDraftsResponse";
import type { CrmSalesIntentActionRequest } from "../../types/generated/CrmSalesIntentActionRequest";
import type { CrmSalesIntentDraftsResponse } from "../../types/generated/CrmSalesIntentDraftsResponse";
import type { CrmSalesIntentProduceRequest } from "../../types/generated/CrmSalesIntentProduceRequest";
import type { CrmSalesIntentProduceResponse } from "../../types/generated/CrmSalesIntentProduceResponse";
import type { CrmSalesIntentUpdateRequest } from "../../types/generated/CrmSalesIntentUpdateRequest";
import type { EmailDraftActionRequest } from "../../types/generated/EmailDraftActionRequest";
import type { EmailDraftManualStageRequest } from "../../types/generated/EmailDraftManualStageRequest";
import type { EmailDraftRewriteRequest } from "../../types/generated/EmailDraftRewriteRequest";
import type { EmailDraftRewriteResponse } from "../../types/generated/EmailDraftRewriteResponse";
import type { EmailDraftUpdateRequest } from "../../types/generated/EmailDraftUpdateRequest";
import type { EmailDraftProduceRequest } from "../../types/generated/EmailDraftProduceRequest";
import type { EmailDraftProduceResponse } from "../../types/generated/EmailDraftProduceResponse";
import type { EmailDraftsResponse } from "../../types/generated/EmailDraftsResponse";
import type { EmailOutboundFollowUpsResponse } from "../../types/generated/EmailOutboundFollowUpsResponse";
import type { EmailOutboundFollowUpActionRequest } from "../../types/generated/EmailOutboundFollowUpActionRequest";
import type { EmailOutboundFollowUpCheckResponse } from "../../types/generated/EmailOutboundFollowUpCheckResponse";
import type { EmailOutboundFollowUpDraftResponse } from "../../types/generated/EmailOutboundFollowUpDraftResponse";
import type { EmailManualFollowUpRequest } from "../../types/generated/EmailManualFollowUpRequest";
import type { EmailTrashRequest } from "../../types/generated/EmailTrashRequest";
import type { EmailTriageConditionCatalogResponse } from "../../types/generated/EmailTriageConditionCatalogResponse";
import type { EmailTriageDryRunRequest } from "../../types/generated/EmailTriageDryRunRequest";
import type { EnrichmentKickoffRequest } from "../../types/generated/EnrichmentKickoffRequest";
import type { EnrichmentKickoffResponse } from "../../types/generated/EnrichmentKickoffResponse";
import type { EnrichmentRunsResponse } from "../../types/generated/EnrichmentRunsResponse";
import type { FollowUpDraftActionRequest } from "../../types/generated/FollowUpDraftActionRequest";
import type { FollowUpDraftManualStageRequest } from "../../types/generated/FollowUpDraftManualStageRequest";
import type { FollowUpDraftUpdateRequest } from "../../types/generated/FollowUpDraftUpdateRequest";
import type { FollowUpDraftProduceRequest } from "../../types/generated/FollowUpDraftProduceRequest";
import type { FollowUpDraftProduceResponse } from "../../types/generated/FollowUpDraftProduceResponse";
import type { FollowUpDraftsResponse } from "../../types/generated/FollowUpDraftsResponse";
import type { HomeDashboardPreferencesUpdateRequest } from "../../types/generated/HomeDashboardPreferencesUpdateRequest";
import type { HomeDashboardResponse } from "../../types/generated/HomeDashboardResponse";
import type { HubSpotDealDiscoveryResponse } from "../../types/generated/HubSpotDealDiscoveryResponse";
import type { HubSpotDealPipelineMappingResponse } from "../../types/generated/HubSpotDealPipelineMappingResponse";
import type { HubSpotDealPipelineMappingSaveRequest } from "../../types/generated/HubSpotDealPipelineMappingSaveRequest";
import type { TaskActionRequest } from "../../types/generated/TaskActionRequest";
import type { TasksResponse } from "../../types/generated/TasksResponse";
import type { TaskStatus } from "../../types/generated/TaskStatus";
import type { EmailTriageDryRunResponse } from "../../types/generated/EmailTriageDryRunResponse";
import type { EmailTriageGmailCategory } from "../../types/generated/EmailTriageGmailCategory";
import type { EmailTriageInboxOptionsResponse } from "../../types/generated/EmailTriageInboxOptionsResponse";
import type { EmailTriageInboxResponse } from "../../types/generated/EmailTriageInboxResponse";
import type { EmailTriageInboxSettingsResponse } from "../../types/generated/EmailTriageInboxSettingsResponse";
import type { EmailTriageInboxSettingsUpdateRequest } from "../../types/generated/EmailTriageInboxSettingsUpdateRequest";
import type { EmailAttachmentEvidenceRequest } from "../../types/generated/EmailAttachmentEvidenceRequest";
import type { EmailAttachmentEvidenceResponse } from "../../types/generated/EmailAttachmentEvidenceResponse";
import type { EmailTriageRuleActionRequest } from "../../types/generated/EmailTriageRuleActionRequest";
import type { EmailTriageRulesListResponse } from "../../types/generated/EmailTriageRulesListResponse";
import type { ReclassifyResponse } from "../../types/generated/ReclassifyResponse";
import type { ReadyzResponse } from "../../types/generated/ReadyzResponse";
import type { EmailTriageRuleUpsertRequest } from "../../types/generated/EmailTriageRuleUpsertRequest";
import type { MutationResponse } from "../../types/generated/MutationResponse";
import type { OwnerReportEmailRequest } from "../../types/generated/OwnerReportEmailRequest";
import type { OwnerReportGenerateResponse } from "../../types/generated/OwnerReportGenerateResponse";
import type { OwnerReportsResponse } from "../../types/generated/OwnerReportsResponse";
import type { OperatorNoteCreateRequest } from "../../types/generated/OperatorNoteCreateRequest";
import type { OperatorNoteCreateResponse } from "../../types/generated/OperatorNoteCreateResponse";
import type { OperatorUserActionRequest } from "../../types/generated/OperatorUserActionRequest";
import type { OperatorUserCreateRequest } from "../../types/generated/OperatorUserCreateRequest";
import type { OperatorUserCreateResponse } from "../../types/generated/OperatorUserCreateResponse";
import type { OperatorUserRotateTokenRequest } from "../../types/generated/OperatorUserRotateTokenRequest";
import type { OperatorUserRotateTokenResponse } from "../../types/generated/OperatorUserRotateTokenResponse";
import type { OperatorUsersResponse } from "../../types/generated/OperatorUsersResponse";
import type { OperatorSessionLoginRequest } from "../../types/generated/OperatorSessionLoginRequest";
import type { OperatorSessionResponse } from "../../types/generated/OperatorSessionResponse";
import type { OperatorSessionVisibilityResponse } from "../../types/generated/OperatorSessionVisibilityResponse";
import type { OutboxRetryRequest } from "../../types/generated/OutboxRetryRequest";
import type { WhoAmIResponse } from "../../types/generated/WhoAmIResponse";
import type { PacketKindsResponse } from "../../types/generated/PacketKindsResponse";
import type { SmartDraftRequest } from "../../types/generated/SmartDraftRequest";
import type { SmartDraftResponse } from "../../types/generated/SmartDraftResponse";
import type { SmartDraftSourceStateRequest } from "../../types/generated/SmartDraftSourceStateRequest";
import type { SmartDraftSourceStateResponse } from "../../types/generated/SmartDraftSourceStateResponse";
import type { ProduceKickoffResponse } from "../../types/generated/ProduceKickoffResponse";
import type { ProduceStatusResponse } from "../../types/generated/ProduceStatusResponse";
import type { ReleaseNoteDismissRequest } from "../../types/generated/ReleaseNoteDismissRequest";
import type { ReleaseNotesResponse } from "../../types/generated/ReleaseNotesResponse";
import type { WorkItemActionRequest } from "../../types/generated/WorkItemActionRequest";
import type { WorkItemAssignRequest } from "../../types/generated/WorkItemAssignRequest";
import type { WorkItemGuidanceUpdateRequest } from "../../types/generated/WorkItemGuidanceUpdateRequest";
import type { WorkItemKindsUpdateRequest } from "../../types/generated/WorkItemKindsUpdateRequest";
import type { WorkItemSourceResponse } from "../../types/generated/WorkItemSourceResponse";
import type { WorkItemStatus } from "../../types/generated/WorkItemStatus";
import type { WorkQueuePoliciesResponse } from "../../types/generated/WorkQueuePoliciesResponse";
import type { WorkQueuePolicyUpsertRequest } from "../../types/generated/WorkQueuePolicyUpsertRequest";
import type { WorkQueueResponse } from "../../types/generated/WorkQueueResponse";


import { request } from "./core";

export const api = {
  readyz(): Promise<ReadyzResponse> {
    return request("/readyz");
  },

  health(): Promise<InstanceHealth> {
    return request("/api/diagnostics/health");
  },

  sessionVisibility(): Promise<OperatorSessionVisibilityResponse> {
    return request("/api/session/visibility");
  },

  login(body: OperatorSessionLoginRequest): Promise<OperatorSessionResponse> {
    return request("/api/session", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  logout(): Promise<OperatorSessionResponse> {
    return request("/api/session/logout", { method: "POST" });
  },

  retryOutboxJob(
    jobId: string,
    body: OutboxRetryRequest,
  ): Promise<MutationResponse> {
    return request(`/api/outbox-jobs/${encodeURIComponent(jobId)}/retry`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  socialProposals(): Promise<SocialPublishingResponse> {
    return request("/api/social-publishing/proposals");
  },

  stageSocialProposal(body: SocialProposalStageRequest): Promise<MutationResponse> {
    return request("/api/social-publishing/proposals", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  updateSocialProposal(
    proposalId: string,
    body: SocialProposalUpdateRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/social-publishing/proposals/${encodeURIComponent(proposalId)}/update`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  actionSocialProposal(
    proposalId: string,
    body: SocialProposalActionRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/social-publishing/proposals/${encodeURIComponent(proposalId)}/action`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  generateSocialProposal(
    sourceId: string,
    body: SocialProposalGenerateRequest,
  ): Promise<SocialGenerationResponse> {
    return request(
      `/api/social-publishing/sources/${encodeURIComponent(sourceId)}/generate`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  generateSocialDraftPreview(
    draftId: string,
    body: SocialDraftPreviewGenerateRequest,
  ): Promise<SocialGenerationResponse> {
    return request(
      `/api/social-publishing/drafts/${encodeURIComponent(draftId)}/generate-preview`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  inbox(params?: {
    categories?: EmailTriageGmailCategory[];
    dashboardCategories?: string[];
    label?: string | null;
    sourceUserId?: string | null;
    crmMatch?: "has_contact" | "no_match" | "has_deal" | null;
    crmDealStages?: string[];
    crmDealPipelines?: string[];
    q?: string | null;
    limit?: number;
  }): Promise<EmailTriageInboxResponse> {
    const search = new URLSearchParams();
    if (params?.categories && params.categories.length > 0) {
      search.set("categories", params.categories.join(","));
    }
    if (params?.dashboardCategories && params.dashboardCategories.length > 0) {
      search.set("dashboard_categories", params.dashboardCategories.join(","));
    }
    if (params?.label) {
      search.set("label", params.label);
    }
    if (params?.sourceUserId) {
      search.set("source_user_id", params.sourceUserId);
    }
    if (params?.crmMatch) {
      search.set("crm_match", params.crmMatch);
    }
    if (params?.crmDealStages && params.crmDealStages.length > 0) {
      search.set("crm_deal_stages", params.crmDealStages.join(","));
    }
    if (params?.crmDealPipelines && params.crmDealPipelines.length > 0) {
      search.set("crm_deal_pipelines", params.crmDealPipelines.join(","));
    }
    if (params?.q?.trim()) {
      search.set("q", params.q.trim());
    }
    if (params?.limit) {
      search.set("limit", String(params.limit));
    }
    const suffix = search.toString();
    return request(`/api/email-triage/inbox${suffix ? `?${suffix}` : ""}`);
  },

  inboxOptions(): Promise<EmailTriageInboxOptionsResponse> {
    return request("/api/email-triage/inbox/options");
  },

  inboxSettings(): Promise<EmailTriageInboxSettingsResponse> {
    return request("/api/email-triage/inbox/settings");
  },

  updateInboxSettings(
    body: EmailTriageInboxSettingsUpdateRequest,
  ): Promise<MutationResponse> {
    return request("/api/email-triage/inbox/settings", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  addInboxFollowUp(
    messageId: string,
    body: EmailManualFollowUpRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/email-triage/inbox/${encodeURIComponent(messageId)}/follow-up`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  smartDraft(body: SmartDraftRequest): Promise<SmartDraftResponse> {
    return request("/api/packet-proposals/smart-draft", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  smartDraftSourceState(
    body: SmartDraftSourceStateRequest,
  ): Promise<SmartDraftSourceStateResponse> {
    return request("/api/packet-proposals/smart-draft/source-state", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  rules(): Promise<EmailTriageRulesListResponse> {
    return request("/api/email-triage/rules");
  },

  reclassify(): Promise<ReclassifyResponse> {
    return request("/api/email-triage/reclassify", { method: "POST" });
  },

  aiRetriageReset(
    body: AiRetriageResetRequest,
  ): Promise<AiRetriageResetResponse> {
    return request("/api/email-triage/ai-retriage-reset", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  upsertRule(body: EmailTriageRuleUpsertRequest): Promise<MutationResponse> {
    return request("/api/email-triage/rules", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  ruleAction(
    ruleId: string,
    body: EmailTriageRuleActionRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/email-triage/rules/${encodeURIComponent(ruleId)}/action`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  dryRun(body: EmailTriageDryRunRequest): Promise<EmailTriageDryRunResponse> {
    return request("/api/email-triage/dry-run", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  conditionCatalog(): Promise<EmailTriageConditionCatalogResponse> {
    return request("/api/email-triage/condition-catalog");
  },

  categories(): Promise<CategoriesListResponse> {
    return request("/api/email-triage/categories");
  },

  upsertCategory(body: CategoryUpsertRequest): Promise<MutationResponse> {
    return request("/api/email-triage/categories", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  deleteCategory(
    categoryId: string,
    body: CategoryDeleteRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/email-triage/categories/${encodeURIComponent(categoryId)}/delete`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  connectorStatus(): Promise<ConnectorStatus> {
    return request("/api/connectors/google/status");
  },

  disconnectGoogle(): Promise<{ disconnected: boolean }> {
    return request<{ disconnected: boolean }>("/api/connectors/google/disconnect", {
      method: "POST",
    });
  },

  googleDriveFolders(q?: string): Promise<GoogleDriveFolderOptionsResponse> {
    const query = q?.trim() ? `?q=${encodeURIComponent(q.trim())}` : "";
    return request(`/api/connectors/google/drive/folders${query}`);
  },

  accountingStatus(): Promise<AccountingConnectorStatus> {
    return request("/api/accounting/status");
  },

  accountingInvoices(
    filter: "open" | "overdue" | "all",
  ): Promise<AccountingInvoicesResponse> {
    return request(`/api/accounting/invoices?filter=${filter}`);
  },

  accountingAging(): Promise<AccountingAgingResponse> {
    return request("/api/accounting/aging");
  },

  accountingFinancials(): Promise<AccountingFinancialsResponse> {
    return request("/api/accounting/financials");
  },

  accountingCustomers(): Promise<AccountingCustomersResponse> {
    return request("/api/accounting/customers");
  },

  accountingSyncNow(): Promise<AccountingSyncNowResponse> {
    return request("/api/accounting/sync", { method: "POST" });
  },

  crmCacheStatus(): Promise<CrmCacheSyncInfo> {
    return request("/api/crm-cache/status");
  },

  crmCacheSyncNow(): Promise<CrmCacheSyncNowResponse> {
    return request("/api/crm-cache/sync", { method: "POST" });
  },

  crmCacheContactsByEmail(email: string): Promise<CrmContactSnapshotsResponse> {
    return request(
      `/api/crm-cache/contacts?email=${encodeURIComponent(email.trim())}`,
    );
  },

  crmCacheContactsByCompany(company: string): Promise<CrmContactSnapshotsResponse> {
    return request(
      `/api/crm-cache/contacts?company=${encodeURIComponent(company.trim())}`,
    );
  },

  crmCacheDealsByContact(contactEmail: string): Promise<CrmDealSnapshotsResponse> {
    return request(
      `/api/crm-cache/deals?contact_email=${encodeURIComponent(contactEmail.trim())}`,
    );
  },

  crmCacheContext(sourceKey: string): Promise<CrmContextResponse> {
    return request(
      `/api/crm-cache/context?source_key=${encodeURIComponent(sourceKey.trim())}`,
    );
  },

  homeDashboard(): Promise<HomeDashboardResponse> {
    return request("/api/home-dashboard");
  },

  hubSpotDealDiscovery(): Promise<HubSpotDealDiscoveryResponse> {
    return request("/api/home-dashboard/hubspot-deals/discovery");
  },

  hubSpotDealMapping(): Promise<HubSpotDealPipelineMappingResponse> {
    return request("/api/home-dashboard/hubspot-deals/mapping");
  },

  updateHubSpotDealMapping(
    body: HubSpotDealPipelineMappingSaveRequest,
  ): Promise<MutationResponse> {
    return request("/api/home-dashboard/hubspot-deals/mapping", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  updateHomeDashboardPreferences(
    body: HomeDashboardPreferencesUpdateRequest,
  ): Promise<MutationResponse> {
    return request("/api/home-dashboard/preferences", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  latestReleaseNote(): Promise<ReleaseNotesResponse> {
    return request("/api/release-notes/latest");
  },

  releaseNotes(): Promise<ReleaseNotesResponse> {
    return request("/api/release-notes");
  },

  dismissReleaseNote(
    releaseNoteId: string,
    body: ReleaseNoteDismissRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/release-notes/${encodeURIComponent(releaseNoteId)}/dismiss`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  customerTierSyncRuns(): Promise<CustomerTierSyncListResponse> {
    return request("/api/customer-tier-sync/runs");
  },

  customerTierSyncPreview(
    body: CustomerTierSyncPreviewRequest,
  ): Promise<CustomerTierSyncRun> {
    return request("/api/customer-tier-sync/preview", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  customerTierSyncApprove(
    runId: string,
    body: CustomerTierSyncApproveRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/customer-tier-sync/runs/${encodeURIComponent(runId)}/approve`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  customerTierSyncReject(
    runId: string,
    body: CustomerTierSyncApproveRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/customer-tier-sync/runs/${encodeURIComponent(runId)}/reject`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  ownerReports(period?: "weekly" | "mtd"): Promise<OwnerReportsResponse> {
    return request(
      period ? `/api/owner-reports?period=${period}` : "/api/owner-reports",
    );
  },

  ownerReportsGenerate(): Promise<OwnerReportGenerateResponse> {
    return request("/api/owner-reports/generate", { method: "POST" });
  },

  searchConsoleStatus(): Promise<SearchConsoleTrafficOverview> {
    return request("/api/search-console/status");
  },

  searchConsoleSyncNow(): Promise<SearchConsoleSyncNowResponse> {
    return request("/api/search-console/sync", { method: "POST" });
  },

  googleAnalyticsSyncNow(): Promise<SearchConsoleSyncNowResponse> {
    return request("/api/google-analytics/sync", { method: "POST" });
  },

  searchConsoleSelectProperty(
    body: SearchConsolePropertySelectRequest,
  ): Promise<MutationResponse> {
    return request("/api/search-console/property", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  emailOwnerReport(
    reportId: string,
    body: OwnerReportEmailRequest,
  ): Promise<MutationResponse> {
    return request(`/api/owner-reports/${encodeURIComponent(reportId)}/email`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  stockforgeStatus(): Promise<StockforgeConnectorStatus> {
    return request("/api/connectors/stockforge/status");
  },

  inventoryStock(): Promise<InventoryStockResponse> {
    return request("/api/inventory/stock");
  },

  inventoryAlerts(): Promise<InventoryAlertsResponse> {
    return request("/api/inventory/alerts");
  },

  inventoryOrders(): Promise<InventoryOrdersResponse> {
    return request("/api/inventory/orders");
  },

  inventoryPurchaseOrders(): Promise<InventoryPurchaseOrdersResponse> {
    return request("/api/inventory/purchase-orders");
  },

  inventorySyncNow(): Promise<InventorySyncNowResponse> {
    return request("/api/inventory/sync", { method: "POST" });
  },

  callInputsStatus(): Promise<CallInputsStatusResponse> {
    return request("/api/call-inputs/status");
  },

  callInputsDriveSettings(): Promise<CallInputsDriveSettingsResponse> {
    return request("/api/call-inputs/drive-settings");
  },

  updateCallInputsDriveSettings(
    body: CallInputsDriveSettingsUpdateRequest,
  ): Promise<MutationResponse> {
    return request("/api/call-inputs/drive-settings", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  callInputs(
    status?: "staged" | "accepted" | "rejected",
  ): Promise<CallInputsResponse> {
    const query = status ? `?status=${encodeURIComponent(status)}` : "";
    return request(`/api/call-inputs${query}`);
  },

  callInputAction(
    callInputId: string,
    body: CallInputActionRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/call-inputs/${encodeURIComponent(callInputId)}/action`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  leadDiscoveryStatus(): Promise<LeadDiscoveryStatusResponse> {
    return request("/api/lead-discovery/status");
  },

  leadFindings(
    status?: "staged" | "accepted" | "rejected",
  ): Promise<LeadFindingsResponse> {
    const query = status ? `?status=${encodeURIComponent(status)}` : "";
    return request(`/api/lead-discovery/findings${query}`);
  },

  leadFindingAction(
    findingId: string,
    body: LeadFindingActionRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/lead-discovery/findings/${encodeURIComponent(findingId)}/action`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  leadFindingStage(body: LeadFindingStageRequest): Promise<MutationResponse> {
    return request("/api/lead-discovery/findings", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  workQueue(params?: {
    status?: WorkItemStatus;
    needsAttention?: boolean;
    attentionLevel?: AttentionLevel;
  }): Promise<WorkQueueResponse> {
    const search = new URLSearchParams();
    if (params?.status) search.set("status", params.status);
    if (params?.needsAttention) search.set("needs_attention", "true");
    if (params?.attentionLevel) search.set("attention_level", params.attentionLevel);
    const query = search.toString();
    return request(`/api/work-queue${query ? `?${query}` : ""}`);
  },

  workItemSource(itemId: string): Promise<WorkItemSourceResponse> {
    return request(`/api/work-queue/${encodeURIComponent(itemId)}/source`);
  },

  stageEmailAttachmentEvidence(
    messageId: string,
    attachmentId: string,
    body: EmailAttachmentEvidenceRequest,
  ): Promise<EmailAttachmentEvidenceResponse> {
    return request(
      `/api/email-triage/inbox/${encodeURIComponent(messageId)}/attachments/${encodeURIComponent(
        attachmentId,
      )}/evidence`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  trashInboxEmail(
    messageId: string,
    body: EmailTrashRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/email-triage/inbox/${encodeURIComponent(messageId)}/trash`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  workItemPacketKinds(
    itemId: string,
    body: WorkItemKindsUpdateRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/work-queue/${encodeURIComponent(itemId)}/packet-kinds`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  workItemProduceGuidance(
    itemId: string,
    body: WorkItemGuidanceUpdateRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/work-queue/${encodeURIComponent(itemId)}/produce-guidance`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  workItemAction(
    itemId: string,
    body: WorkItemActionRequest,
  ): Promise<MutationResponse> {
    return request(`/api/work-queue/${encodeURIComponent(itemId)}/action`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  workItemAssignment(
    itemId: string,
    body: WorkItemAssignRequest,
  ): Promise<MutationResponse> {
    return request(`/api/work-queue/${encodeURIComponent(itemId)}/assignment`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  aiUsage(): Promise<AiUsageResponse> {
    return request("/api/ai-usage");
  },

  llmSettings(): Promise<LlmRouteSettingsResponse> {
    return request("/api/llm-settings");
  },

  updateLlmSettings(
    body: LlmRouteSettingsUpdateRequest,
  ): Promise<MutationResponse> {
    return request("/api/llm-settings", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  claudeSubscriptionStatus(): Promise<ClaudeSubscriptionStatus> {
    return request("/api/llm-settings/claude-subscription");
  },

  startClaudeSubscriptionAuth(
    body: ClaudeSubscriptionAuthStartRequest,
  ): Promise<ClaudeSubscriptionAuthStartResponse> {
    return request("/api/llm-settings/claude-subscription/start", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  completeClaudeSubscriptionAuth(
    body: ClaudeSubscriptionAuthCompleteRequest,
  ): Promise<MutationResponse> {
    return request("/api/llm-settings/claude-subscription/complete", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  invoiceSettings(): Promise<InvoiceSettingsResponse> {
    return request("/api/invoice-drafts/settings");
  },

  updateInvoiceSettings(
    body: InvoiceSettingsUpdateRequest,
  ): Promise<MutationResponse> {
    return request("/api/invoice-drafts/settings", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  adminSettings(): Promise<AdminSettingsResponse> {
    return request("/api/admin/settings");
  },

  updateAdminSetting(
    varName: string,
    body: AdminSettingUpdateRequest,
  ): Promise<MutationResponse> {
    return request(`/api/admin/settings/${encodeURIComponent(varName)}`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  clearAdminSetting(
    varName: string,
    body: AdminSettingClearRequest,
  ): Promise<MutationResponse> {
    return request(`/api/admin/settings/${encodeURIComponent(varName)}`, {
      method: "DELETE",
      body: JSON.stringify(body),
    });
  },

  debugDiagnostics(): Promise<DebugDiagnosticsResponse> {
    return request("/api/debug");
  },

  debugSpawnAgent(
    body: DebugSpawnAgentRequest,
  ): Promise<DebugSpawnAgentResponse> {
    return request("/api/debug/spawn-agent", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  launchAgent(
    itemId: string,
    body: LaunchAgentRequest,
  ): Promise<LaunchAgentResponse> {
    return request(
      `/api/work-queue/${encodeURIComponent(itemId)}/launch-agent`,
      {
        method: "POST",
        body: JSON.stringify(body),
      },
    );
  },

  calendarDrafts(itemId?: string): Promise<CalendarDraftsResponse> {
    const query = itemId ? `?item_id=${encodeURIComponent(itemId)}` : "";
    return request(`/api/calendar-drafts${query}`);
  },

  calendarOptions(): Promise<CalendarListResponse> {
    return request("/api/calendar-drafts/calendars");
  },

  produceStatus(
    itemId: string,
    kind: string,
    idempotencyKey: string,
  ): Promise<ProduceStatusResponse> {
    const query = new URLSearchParams({
      item_id: itemId,
      kind,
      idempotency_key: idempotencyKey,
    });
    return request(`/api/produce/status?${query.toString()}`);
  },

  produceCalendarDraft(
    body: CalendarDraftProduceRequest,
  ): Promise<CalendarDraftProduceResponse | ProduceKickoffResponse> {
    return request("/api/calendar-drafts/produce", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  updateCalendarDraft(
    draftId: string,
    body: CalendarDraftUpdateRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/calendar-drafts/${encodeURIComponent(draftId)}/update`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  calendarDraftAction(
    draftId: string,
    body: CalendarDraftActionRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/calendar-drafts/${encodeURIComponent(draftId)}/action`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  crmDrafts(itemId?: string): Promise<CrmDraftsResponse> {
    const query = itemId ? `?item_id=${encodeURIComponent(itemId)}` : "";
    return request(`/api/crm-drafts${query}`);
  },

  produceCrmDraft(
    body: CrmDraftProduceRequest,
  ): Promise<CrmDraftProduceResponse | ProduceKickoffResponse> {
    return request("/api/crm-drafts/produce", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  updateCrmDraft(
    draftId: string,
    body: CrmDraftUpdateRequest,
  ): Promise<MutationResponse> {
    return request(`/api/crm-drafts/${encodeURIComponent(draftId)}/update`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  crmDraftAction(
    draftId: string,
    body: CrmDraftActionRequest,
  ): Promise<MutationResponse> {
    return request(`/api/crm-drafts/${encodeURIComponent(draftId)}/action`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },
  crmRecordDrafts(itemId?: string): Promise<CrmRecordDraftsResponse> {
    const query = itemId ? `?item_id=${encodeURIComponent(itemId)}` : "";
    return request(`/api/crm-record-drafts${query}`);
  },

  enrichmentRuns(params: {
    sliceId?: string;
    draftId?: string;
    itemId?: string;
    limit?: number;
  }): Promise<EnrichmentRunsResponse> {
    const query = new URLSearchParams();
    if (params.sliceId) query.set("slice_id", params.sliceId);
    if (params.draftId) query.set("draft_id", params.draftId);
    if (params.itemId) query.set("item_id", params.itemId);
    if (params.limit != null) query.set("limit", String(params.limit));
    const suffix = query.toString();
    return request(`/api/enrichment/runs${suffix ? `?${suffix}` : ""}`);
  },

  enrichCrmRecordDraft(
    draftId: string,
    body: EnrichmentKickoffRequest,
  ): Promise<EnrichmentKickoffResponse> {
    return request(`/api/crm-record-drafts/${encodeURIComponent(draftId)}/enrich`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  produceCrmRecordDraft(
    body: CrmRecordDraftProduceRequest,
  ): Promise<CrmRecordDraftProduceResponse | ProduceKickoffResponse> {
    return request("/api/crm-record-drafts/produce", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  updateCrmRecordDraft(
    draftId: string,
    body: CrmRecordDraftUpdateRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/crm-record-drafts/${encodeURIComponent(draftId)}/update`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  crmRecordDraftAction(
    draftId: string,
    body: CrmRecordDraftActionRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/crm-record-drafts/${encodeURIComponent(draftId)}/action`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  crmSalesIntentDrafts(itemId?: string): Promise<CrmSalesIntentDraftsResponse> {
    const query = itemId ? `?item_id=${encodeURIComponent(itemId)}` : "";
    return request(`/api/crm-sales-intent${query}`);
  },

  produceCrmSalesIntent(
    body: CrmSalesIntentProduceRequest,
  ): Promise<CrmSalesIntentProduceResponse | ProduceKickoffResponse> {
    return request("/api/crm-sales-intent/produce", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  updateCrmSalesIntent(
    draftId: string,
    body: CrmSalesIntentUpdateRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/crm-sales-intent/${encodeURIComponent(draftId)}/update`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  crmSalesIntentAction(
    draftId: string,
    body: CrmSalesIntentActionRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/crm-sales-intent/${encodeURIComponent(draftId)}/action`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  claimDrafts(itemId?: string): Promise<ClaimDraftsResponse> {
    const query = itemId ? `?item_id=${encodeURIComponent(itemId)}` : "";
    return request(`/api/claim-drafts${query}`);
  },

  produceClaimDraft(
    body: ClaimDraftProduceRequest,
  ): Promise<ClaimDraftProduceResponse | ProduceKickoffResponse> {
    return request("/api/claim-drafts/produce", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  updateClaimDraft(
    draftId: string,
    body: ClaimDraftUpdateRequest,
  ): Promise<MutationResponse> {
    return request(`/api/claim-drafts/${encodeURIComponent(draftId)}/update`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  claimDraftAction(
    draftId: string,
    body: ClaimDraftActionRequest,
  ): Promise<MutationResponse> {
    return request(`/api/claim-drafts/${encodeURIComponent(draftId)}/action`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  contentDrafts(itemId?: string): Promise<ContentDraftsResponse> {
    const query = itemId ? `?item_id=${encodeURIComponent(itemId)}` : "";
    return request(`/api/content-drafts${query}`);
  },

  produceContentDraft(
    body: ContentDraftProduceRequest,
  ): Promise<ContentDraftProduceResponse | ProduceKickoffResponse> {
    return request("/api/content-drafts/produce", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  updateContentDraft(
    draftId: string,
    body: ContentDraftUpdateRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/content-drafts/${encodeURIComponent(draftId)}/update`,
      {
        method: "POST",
        body: JSON.stringify(body),
      },
    );
  },

  contentDraftAction(
    draftId: string,
    body: ContentDraftActionRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/content-drafts/${encodeURIComponent(draftId)}/action`,
      {
        method: "POST",
        body: JSON.stringify(body),
      },
    );
  },

  publishContentDraft(
    draftId: string,
    body: ContentDraftPublishRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/content-drafts/${encodeURIComponent(draftId)}/publish`,
      {
        method: "POST",
        body: JSON.stringify(body),
      },
    );
  },

  contentDraftOverlap(draftId: string): Promise<ContentDraftOverlapResponse> {
    return request(
      `/api/content-plans/draft-overlap/${encodeURIComponent(draftId)}`,
    );
  },

  contentPlanItems(status?: ContentPlanStatus): Promise<ContentPlanItemsResponse> {
    const query = status ? `?status=${encodeURIComponent(status)}` : "";
    return request(`/api/content-plans/items${query}`);
  },

  contentCampaignWorkspace(
    planItemId: string,
  ): Promise<ContentCampaignWorkspaceResponse> {
    return request(
      `/api/content-plans/items/${encodeURIComponent(planItemId)}/campaign`,
    );
  },

  generateContentCampaign(
    planItemId: string,
    body: ContentCampaignGenerateRequest,
  ): Promise<ContentDraftProduceResponse | ProduceKickoffResponse> {
    return request(
      `/api/content-plans/items/${encodeURIComponent(planItemId)}/generate`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  publishContentCampaign(
    planItemId: string,
    body: ContentCampaignPublishRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/content-plans/items/${encodeURIComponent(planItemId)}/publish-campaign`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  createContentPlanItem(
    body: ContentPlanItemCreateRequest,
  ): Promise<MutationResponse> {
    return request("/api/content-plans/items", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  updateContentPlanItem(
    planItemId: string,
    body: ContentPlanItemUpdateRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/content-plans/items/${encodeURIComponent(planItemId)}/update`,
      {
        method: "POST",
        body: JSON.stringify(body),
      },
    );
  },

  queueContentPlanItem(
    planItemId: string,
    body: ContentPlanItemQueueRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/content-plans/items/${encodeURIComponent(planItemId)}/queue`,
      {
        method: "POST",
        body: JSON.stringify(body),
      },
    );
  },

  checkContentPlanItem(
    planItemId: string,
    body: ContentPlanItemCheckRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/content-plans/items/${encodeURIComponent(planItemId)}/check`,
      {
        method: "POST",
        body: JSON.stringify(body),
      },
    );
  },

  markContentPlanPublished(
    planItemId: string,
    body: ContentPlanItemMarkPublishedRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/content-plans/items/${encodeURIComponent(planItemId)}/mark-published`,
      {
        method: "POST",
        body: JSON.stringify(body),
      },
    );
  },

  contentInventory(status?: ContentInventoryStatus): Promise<ContentInventoryResponse> {
    const query = status ? `?status=${encodeURIComponent(status)}` : "";
    return request(`/api/content-plans/inventory${query}`);
  },

  addContentInventory(
    body: ContentInventoryManualAddRequest,
  ): Promise<MutationResponse> {
    return request("/api/content-plans/inventory", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  refreshContentInventory(
    body: ContentInventoryRefreshRequest,
  ): Promise<MutationResponse> {
    return request("/api/content-plans/inventory/refresh", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  archiveContentInventory(
    inventoryId: string,
    body: ContentInventoryArchiveRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/content-plans/inventory/${encodeURIComponent(inventoryId)}/archive`,
      {
        method: "POST",
        body: JSON.stringify(body),
      },
    );
  },

  driveCorpusStatus(): Promise<DriveCorpusStatus> {
    return request("/api/drive-corpus/status");
  },

  updateDriveCorpusSettings(
    body: DriveCorpusSettingsUpdateRequest,
  ): Promise<DriveCorpusSettingsUpdateResponse> {
    return request("/api/drive-corpus/settings", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  driveCorpusSyncNow(): Promise<DriveSyncNowResponse> {
    return request("/api/drive-corpus/sync", { method: "POST" });
  },

  driveCorpusSearch(q: string, limit = 10): Promise<DriveSearchResponse> {
    return request(
      `/api/drive-corpus/search?q=${encodeURIComponent(q)}&limit=${limit}`,
    );
  },

  invoiceDrafts(itemId?: string): Promise<InvoiceDraftsResponse> {
    const query = itemId ? `?item_id=${encodeURIComponent(itemId)}` : "";
    return request(`/api/invoice-drafts${query}`);
  },

  enrichInvoiceDraft(
    draftId: string,
    body: EnrichmentKickoffRequest,
  ): Promise<EnrichmentKickoffResponse> {
    return request(`/api/invoice-drafts/${encodeURIComponent(draftId)}/enrich`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  produceInvoiceDraft(
    body: InvoiceDraftProduceRequest,
  ): Promise<InvoiceDraftProduceResponse | { producing: boolean }> {
    return request("/api/invoice-drafts/produce", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  updateInvoiceDraft(
    draftId: string,
    body: InvoiceDraftUpdateRequest,
  ): Promise<MutationResponse> {
    return request(`/api/invoice-drafts/${encodeURIComponent(draftId)}/update`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  invoiceDraftAction(
    draftId: string,
    body: InvoiceDraftActionRequest,
  ): Promise<MutationResponse> {
    return request(`/api/invoice-drafts/${encodeURIComponent(draftId)}/action`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  ledgerDrafts(itemId?: string): Promise<LedgerDraftsResponse> {
    const query = itemId ? `?item_id=${encodeURIComponent(itemId)}` : "";
    return request(`/api/ledger-drafts${query}`);
  },

  produceLedgerDraft(
    body: LedgerDraftProduceRequest,
  ): Promise<LedgerDraftProduceResponse | { producing: boolean }> {
    return request("/api/ledger-drafts/produce", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  updateLedgerDraft(
    draftId: string,
    body: LedgerDraftUpdateRequest,
  ): Promise<MutationResponse> {
    return request(`/api/ledger-drafts/${encodeURIComponent(draftId)}/update`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  ledgerDraftAction(
    draftId: string,
    body: LedgerDraftActionRequest,
  ): Promise<MutationResponse> {
    return request(`/api/ledger-drafts/${encodeURIComponent(draftId)}/action`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },


  emailDrafts(itemId?: string): Promise<EmailDraftsResponse> {
    const query = itemId ? `?item_id=${encodeURIComponent(itemId)}` : "";
    return request(`/api/email-drafts${query}`);
  },

  produceEmailDraft(
    body: EmailDraftProduceRequest,
  ): Promise<EmailDraftProduceResponse | ProduceKickoffResponse> {
    return request("/api/email-drafts/produce", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  stageManualEmailDraft(
    body: EmailDraftManualStageRequest,
  ): Promise<EmailDraftProduceResponse> {
    return request("/api/email-drafts/manual", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  updateEmailDraft(
    draftId: string,
    body: EmailDraftUpdateRequest,
  ): Promise<MutationResponse> {
    return request(`/api/email-drafts/${encodeURIComponent(draftId)}/update`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  rewriteEmailDraft(
    draftId: string,
    body: EmailDraftRewriteRequest,
  ): Promise<EmailDraftRewriteResponse> {
    return request(`/api/email-drafts/${encodeURIComponent(draftId)}/rewrite`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  emailDraftAction(
    draftId: string,
    body: EmailDraftActionRequest,
  ): Promise<MutationResponse> {
    return request(`/api/email-drafts/${encodeURIComponent(draftId)}/action`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  /** Outbound email follow-up workflows (issue #185). */
  emailFollowUps(
    status?: "open" | "resolved" | "all",
  ): Promise<EmailOutboundFollowUpsResponse> {
    const query = status ? `?status=${status}` : "";
    return request(`/api/email-drafts/follow-ups${query}`);
  },

  /** Manual reconciliation — idempotent, returns the (possibly updated) summary. */
  emailFollowUpCheck(
    followUpId: string,
    body: EmailOutboundFollowUpActionRequest,
  ): Promise<EmailOutboundFollowUpCheckResponse> {
    return request(
      `/api/email-drafts/follow-ups/${encodeURIComponent(followUpId)}/check`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  /** Open an email_draft_reply work item for an explicit follow-up reply. */
  emailFollowUpDraft(
    followUpId: string,
    body: EmailOutboundFollowUpActionRequest,
  ): Promise<EmailOutboundFollowUpDraftResponse> {
    return request(
      `/api/email-drafts/follow-ups/${encodeURIComponent(followUpId)}/draft`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  followUpDrafts(itemId?: string): Promise<FollowUpDraftsResponse> {
    const query = itemId ? `?item_id=${encodeURIComponent(itemId)}` : "";
    return request(`/api/follow-up-drafts${query}`);
  },

  produceFollowUpDraft(
    body: FollowUpDraftProduceRequest,
  ): Promise<FollowUpDraftProduceResponse | ProduceKickoffResponse> {
    return request("/api/follow-up-drafts/produce", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  stageManualFollowUpDraft(
    body: FollowUpDraftManualStageRequest,
  ): Promise<FollowUpDraftProduceResponse> {
    return request("/api/follow-up-drafts/manual", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  updateFollowUpDraft(
    draftId: string,
    body: FollowUpDraftUpdateRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/follow-up-drafts/${encodeURIComponent(draftId)}/update`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  followUpDraftAction(
    draftId: string,
    body: FollowUpDraftActionRequest,
  ): Promise<MutationResponse> {
    return request(
      `/api/follow-up-drafts/${encodeURIComponent(draftId)}/action`,
      { method: "POST", body: JSON.stringify(body) },
    );
  },

  tasks(status?: TaskStatus, today?: string): Promise<TasksResponse> {
    const params = new URLSearchParams();
    if (status) params.set("status", status);
    if (today) params.set("today", today);
    const query = params.size > 0 ? `?${params.toString()}` : "";
    return request(`/api/tasks${query}`);
  },

  taskAction(taskId: string, body: TaskActionRequest): Promise<MutationResponse> {
    return request(`/api/tasks/${encodeURIComponent(taskId)}/action`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  createOperatorNote(
    body: OperatorNoteCreateRequest,
  ): Promise<OperatorNoteCreateResponse> {
    return request("/api/operator-notes", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  whoami(): Promise<WhoAmIResponse> {
    return request("/api/me");
  },

  users(includeArchived = false): Promise<OperatorUsersResponse> {
    const query = includeArchived ? "?include_archived=true" : "";
    return request(`/api/users${query}`);
  },

  createUser(body: OperatorUserCreateRequest): Promise<OperatorUserCreateResponse> {
    return request("/api/users", { method: "POST", body: JSON.stringify(body) });
  },

  userAction(
    userId: string,
    body: OperatorUserActionRequest,
  ): Promise<MutationResponse> {
    return request(`/api/users/${encodeURIComponent(userId)}/action`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  rotateUserToken(
    userId: string,
    body: OperatorUserRotateTokenRequest,
  ): Promise<OperatorUserRotateTokenResponse> {
    return request(`/api/users/${encodeURIComponent(userId)}/rotate-token`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  setUserDefaultCalendar(
    userId: string,
    body: OperatorUserDefaultCalendarRequest,
  ): Promise<MutationResponse> {
    return request(`/api/users/${encodeURIComponent(userId)}/default-calendar`, {
      method: "POST",
      body: JSON.stringify(body),
    });
  },

  packetKinds(): Promise<PacketKindsResponse> {
    return request("/api/work-queue/packet-kinds");
  },

  workQueuePolicies(): Promise<WorkQueuePoliciesResponse> {
    return request("/api/work-queue/policies");
  },

  upsertWorkQueuePolicy(
    body: WorkQueuePolicyUpsertRequest,
  ): Promise<MutationResponse> {
    return request("/api/work-queue/policies", {
      method: "POST",
      body: JSON.stringify(body),
    });
  },
};
