export type AppTab =
  | "home"
  | "inbox"
  | "queue"
  | "tasks"
  | "calls"
  | "leads"
  | "plans"
  | "social"
  | "inventory"
  | "accounting"
  | "analytics"
  | "reports"
  | "rules"
  | "categories"
  | "usage"
  | "settings"
  | "debug"
  | "users";

export type SectionGroup = "Work" | "Records" | "Automation" | "System";
export type SettingsSectionId =
  | "google"
  | "dashboard"
  | "hubspot_deals"
  | "inbox"
  | "ai"
  | "content_generation"
  | "invoicing"
  | "calls"
  | "system";

export interface OperatorSection {
  id: string;
  label: string;
  group: SectionGroup;
  tab: AppTab;
  slice?: string;
  commandKeywords?: string;
  nav: boolean;
  settingsSection?: SettingsSectionId;
}

// Debug is intentionally excluded: it is enabled by probing diagnostics access,
// not by a client overlay slice, so it is outside the section coverage set.
export const SECTIONS: readonly OperatorSection[] = [
  {
    id: "home",
    label: "Home",
    group: "Work",
    tab: "home",
    slice: "home_dashboard",
    commandKeywords: "home dashboard overview",
    nav: true,
  },
  {
    id: "inbox",
    label: "Inbox",
    group: "Work",
    tab: "inbox",
    slice: "email_triage",
    commandKeywords: "inbox mail email",
    nav: true,
  },
  {
    id: "queue",
    label: "Queue",
    group: "Work",
    tab: "queue",
    slice: "work_queue",
    commandKeywords: "queue work items",
    nav: true,
  },
  {
    id: "tasks",
    label: "Tasks",
    group: "Work",
    tab: "tasks",
    slice: "follow_up_tasks",
    commandKeywords: "tasks follow-up",
    nav: true,
  },
  {
    id: "calls",
    label: "Calls",
    group: "Work",
    tab: "calls",
    slice: "call_inputs",
    commandKeywords: "calls transcripts recordings call inputs",
    nav: true,
  },
  {
    id: "leads",
    label: "Leads",
    group: "Work",
    tab: "leads",
    slice: "lead_discovery",
    commandKeywords: "leads discovery monitoring sources",
    nav: true,
  },
  {
    id: "plans",
    label: "Content",
    group: "Work",
    tab: "plans",
    slice: "content_plans",
    commandKeywords: "content campaigns plans research writing publishing inventory",
    nav: true,
  },
  {
    id: "social",
    label: "Social",
    group: "Work",
    tab: "social",
    slice: "social_publishing",
    commandKeywords: "social publishing Buffer posts channels approval",
    nav: true,
  },
  {
    id: "inventory",
    label: "Inventory",
    group: "Records",
    tab: "inventory",
    slice: "inventory",
    commandKeywords: "inventory stock orders",
    nav: true,
  },
  {
    id: "accounting",
    label: "Accounting",
    group: "Records",
    tab: "accounting",
    slice: "accounting",
    commandKeywords: "accounting invoices financials",
    nav: true,
  },
  {
    id: "analytics",
    label: "Web Analytics",
    group: "Records",
    tab: "analytics",
    slice: "search_console",
    commandKeywords: "web analytics search console ga4 traffic",
    nav: true,
  },
  {
    id: "reports",
    label: "Reports",
    group: "Records",
    tab: "reports",
    slice: "owner_reports",
    commandKeywords: "reports owner digest metrics",
    nav: true,
  },
  {
    id: "rules",
    label: "Rules",
    group: "Automation",
    tab: "rules",
    slice: "email_triage",
    commandKeywords: "rules triage automation",
    nav: true,
  },
  {
    id: "categories",
    label: "Categories",
    group: "Automation",
    tab: "categories",
    slice: "email_triage",
    commandKeywords: "categories classify",
    nav: true,
  },
  {
    id: "settings",
    label: "Settings",
    group: "System",
    tab: "settings",
    commandKeywords: "settings ai model routing config",
    nav: true,
  },
  {
    id: "usage",
    label: "AI Usage",
    group: "System",
    tab: "usage",
    slice: "ai_usage",
    commandKeywords: "ai usage tokens cost",
    nav: true,
  },
  {
    id: "users",
    label: "Users",
    group: "System",
    tab: "users",
    slice: "operator_users",
    commandKeywords: "users operator accounts",
    nav: true,
  },
  {
    id: "settings-google",
    label: "Google",
    group: "System",
    tab: "settings",
    slice: "google_connector",
    nav: false,
    settingsSection: "google",
  },
  {
    id: "settings-dashboard",
    label: "Dashboard",
    group: "System",
    tab: "settings",
    slice: "home_dashboard",
    nav: false,
    settingsSection: "dashboard",
  },
  {
    id: "settings-hubspot-deals",
    label: "HubSpot Deals",
    group: "System",
    tab: "settings",
    slice: "home_dashboard",
    nav: false,
    settingsSection: "hubspot_deals",
  },
  {
    id: "settings-inbox",
    label: "Inbox",
    group: "System",
    tab: "settings",
    slice: "email_triage",
    nav: false,
    settingsSection: "inbox",
  },
  {
    id: "settings-ai",
    label: "AI",
    group: "System",
    tab: "settings",
    slice: "ai_usage",
    nav: false,
    settingsSection: "ai",
  },
  {
    id: "settings-content-generation",
    label: "Content generation",
    group: "System",
    tab: "settings",
    slice: "drive_corpus",
    nav: false,
    settingsSection: "content_generation",
  },
  {
    id: "settings-invoicing",
    label: "Invoicing",
    group: "System",
    tab: "settings",
    slice: "invoice_drafts",
    nav: false,
    settingsSection: "invoicing",
  },
  {
    id: "settings-calls",
    label: "Audio recordings",
    group: "System",
    tab: "settings",
    slice: "call_inputs",
    nav: false,
    settingsSection: "calls",
  },
  {
    id: "settings-system",
    label: "System",
    group: "System",
    tab: "settings",
    slice: "admin_settings",
    nav: false,
    settingsSection: "system",
  },
] as const;

export const NAV_SECTIONS = SECTIONS.filter((section) => section.nav);

export const SETTINGS_SECTIONS = SECTIONS.filter(
  (section): section is OperatorSection & { settingsSection: SettingsSectionId; slice: string } =>
    section.settingsSection !== undefined && section.slice !== undefined,
);

export function sectionEnabled(
  section: Pick<OperatorSection, "slice">,
  enabledSlices: readonly string[] | null,
): boolean {
  return (
    section.slice === undefined ||
    (enabledSlices !== null &&
      (enabledSlices.length === 0 || enabledSlices.includes(section.slice)))
  );
}

export function sliceForTab(tab: AppTab): string | undefined {
  return NAV_SECTIONS.find((section) => section.tab === tab)?.slice;
}
