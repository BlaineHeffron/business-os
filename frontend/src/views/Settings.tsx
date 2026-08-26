import { useEffect, useMemo, useState } from "react";
import {
  SETTINGS_SECTIONS,
  sectionEnabled,
  type SettingsSectionId,
} from "../lib/sections";
import { helpTopicIdForSettingsSection, type HelpTopic } from "../help/topics";
import SectionHelpButton from "../components/SectionHelpButton";
import { AiSettings } from "./settings/AiSettings";
import { CallSettings } from "./settings/CallSettings";
import { DriveCorpusSettings } from "./settings/DriveCorpusSettings";
import { GoogleSettings } from "./settings/GoogleSettings";
import { HomeDashboardSettings } from "./settings/HomeDashboardSettings";
import { HubSpotDealsSettings } from "./settings/HubSpotDealsSettings";
import { InboxSettings } from "./settings/InboxSettings";
import { InvoiceSettings } from "./settings/InvoiceSettings";
import { SystemSettings } from "./settings/SystemSettings";

type Section = SettingsSectionId;

export default function Settings({
  onUnauthorized,
  enabledSlices,
  helpTopics,
  onOpenHelpTopic,
  initialSection,
  onConnectorChanged,
  aiTriageEnabled,
}: {
  onUnauthorized: () => void;
  enabledSlices: string[] | null;
  helpTopics: readonly HelpTopic[];
  onOpenHelpTopic: (topicId: string) => void;
  initialSection?: SettingsSectionId | null;
  onConnectorChanged?: () => void;
  aiTriageEnabled: boolean;
}) {
  const [section, setSection] = useState<Section>(initialSection ?? "google");
  // Wait for readyz before mounting section bodies; otherwise a disabled
  // settings section can briefly fire a 404ing API request on startup.
  const visible = useMemo(
    () =>
      enabledSlices === null
        ? []
        : SETTINGS_SECTIONS.filter((s) => sectionEnabled(s, enabledSlices)),
    [enabledSlices],
  );
  const activeSection = visible.some((s) => s.settingsSection === section)
    ? section
    : (visible[0]?.settingsSection ?? null);
  const helpTopicId = helpTopicIdForSettingsSection(helpTopics, activeSection);

  // Keep the active section valid as visibility resolves (e.g. AI-only client
  // landing on a defaulted-but-hidden section).
  useEffect(() => {
    if (activeSection !== null && activeSection !== section) {
      setSection(activeSection);
    }
  }, [activeSection, section]);

  useEffect(() => {
    if (
      initialSection &&
      visible.some((s) => s.settingsSection === initialSection) &&
      initialSection !== section
    ) {
      setSection(initialSection);
    }
  }, [initialSection, section, visible]);

  return (
    <div className="flex flex-col gap-4">
      <div className="surface-section-head surface-head-zinc flex items-center justify-between">
        <h1 className="text-lg font-semibold text-zinc-100">Settings</h1>
        <span className="text-xs text-zinc-400">instance configuration</span>
      </div>
      <div className="flex flex-col gap-4 md:flex-row">
        <nav aria-label="Settings sections" className="flex max-w-full flex-row gap-1 overflow-x-auto pb-1 md:w-40 md:flex-none md:flex-col md:overflow-visible md:pb-0">
          {visible.map((s) => (
            <div key={s.id} className="flex flex-none items-center gap-1">
              <button
                onClick={() => setSection(s.settingsSection)}
                aria-current={section === s.settingsSection ? "page" : undefined}
                className={`min-w-0 flex-1 whitespace-nowrap rounded-md px-3 py-1.5 text-left text-sm font-medium transition focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70 md:whitespace-normal ${
                  section === s.settingsSection
                    ? "bg-zinc-800 text-zinc-100"
                    : "text-zinc-400 hover:bg-zinc-900 hover:text-zinc-200"
                }`}
              >
                {s.label}
              </button>
              {s.settingsSection === activeSection ? (
                <SectionHelpButton
                  topicId={helpTopicId}
                  onOpenHelp={onOpenHelpTopic}
                  label={`Open help for ${s.label} settings`}
                />
              ) : null}
            </div>
          ))}
        </nav>
        <div className="min-w-0 flex-1">
          {enabledSlices === null ? (
            <div className="text-sm text-zinc-400">Loading…</div>
          ) : activeSection === "google" ? (
            <GoogleSettings
              onUnauthorized={onUnauthorized}
              onChanged={onConnectorChanged}
            />
          ) : activeSection === "dashboard" ? (
            <HomeDashboardSettings onUnauthorized={onUnauthorized} />
          ) : activeSection === "hubspot_deals" ? (
            <HubSpotDealsSettings onUnauthorized={onUnauthorized} />
          ) : activeSection === "inbox" ? (
            <InboxSettings
              onUnauthorized={onUnauthorized}
              aiTriageEnabled={aiTriageEnabled}
            />
          ) : activeSection === "ai" ? (
            <AiSettings onUnauthorized={onUnauthorized} />
          ) : activeSection === "system" ? (
            <SystemSettings onUnauthorized={onUnauthorized} />
          ) : activeSection === "content_generation" ? (
            <DriveCorpusSettings onUnauthorized={onUnauthorized} />
          ) : activeSection === "calls" ? (
            <CallSettings onUnauthorized={onUnauthorized} />
          ) : activeSection === "invoicing" ? (
            <InvoiceSettings onUnauthorized={onUnauthorized} />
          ) : (
            <div className="text-sm text-zinc-400">No settings available.</div>
          )}
        </div>
      </div>
    </div>
  );
}
