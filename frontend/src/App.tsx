import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ConnectorStatus } from "./types/generated/ConnectorStatus";
import type { EmailTriageRule } from "./types/generated/EmailTriageRule";
import { api, errorMessage, isUnauthorized } from "./lib/api";
import { CategoriesProvider } from "./lib/categories";
import { dispatchAppCommand } from "./lib/commands";
import type { AppCommand } from "./lib/commandTypes";
import { isMac } from "./lib/commands";
import {
  GETTING_AROUND_TOPIC_ID,
  articleBackedHelpTopics,
  buildHelpTopics,
  helpTopicIdForTab,
} from "./help/topics";
import {
  NAV_SECTIONS,
  sectionEnabled,
  sliceForTab,
  type AppTab,
  type SectionGroup,
  type SettingsSectionId,
} from "./lib/sections";
import CommandPalette from "./components/CommandPalette";
import ConnectorBanner from "./components/ConnectorBanner";
import ReleaseBanner from "./components/ReleaseBanner";
import HelpDrawer from "./components/HelpDrawer";
import Categories from "./views/Categories";
import Inbox from "./views/Inbox";
import Queue from "./views/Queue";
import Rules from "./views/Rules";
import CallInputs from "./views/CallInputs";
import Leads from "./views/Leads";
import ContentPlans from "./views/ContentPlans";
import SocialPublishing from "./views/SocialPublishing";
import Accounting from "./views/Accounting";
import Inventory from "./views/Inventory";
import WebAnalytics from "./views/WebAnalytics";
import Reports from "./views/Reports";
import Tasks from "./views/Tasks";
import Usage from "./views/Usage";
import Settings from "./views/Settings";
import Debug from "./views/Debug";
import Users from "./views/Users";
import Button from "./components/ui/Button";
import ThemeToggle from "./components/ThemeToggle";
import Home from "./views/Home";
import OutputComposer, { type OutputKind } from "./components/OutputComposer";

type Tab = AppTab;
type TargetedAppCommand = AppCommand & { targetTab?: Tab };
const SETTINGS_SECTION_IDS: readonly SettingsSectionId[] = [
  "google",
  "dashboard",
  "hubspot_deals",
  "inbox",
  "ai",
  "content_generation",
  "invoicing",
  "calls",
  "system",
];

const PAGE_TINT_BY_TAB: Partial<Record<Tab, string>> = {
  home: "app-page-tint-home",
  inbox: "app-page-tint-sky",
  queue: "app-page-tint-violet",
  tasks: "app-page-tint-amber",
  calls: "app-page-tint-sky",
  leads: "app-page-tint-orange",
  plans: "app-page-tint-zinc",
  social: "app-page-tint-violet",
  inventory: "app-page-tint-teal",
  accounting: "app-page-tint-emerald",
  analytics: "app-page-tint-sky",
  reports: "app-page-tint-violet",
  rules: "app-page-tint-violet",
  categories: "app-page-tint-zinc",
  settings: "app-page-tint-zinc",
  usage: "app-page-tint-amber",
  users: "app-page-tint-emerald",
  debug: "app-page-tint-rose",
};

function isSettingsSectionId(value: string | null | undefined): value is SettingsSectionId {
  return SETTINGS_SECTION_IDS.includes(value as SettingsSectionId);
}

function TokenPopover({
  open,
  onClose,
  onSaved,
  onSignedOut,
  placement = "above",
}: {
  open: boolean;
  onClose: () => void;
  onSaved: () => void;
  onSignedOut: () => void;
  placement?: "above" | "below";
}) {
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (open) {
      setValue("");
      setError(null);
    }
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const handler = (e: MouseEvent) => {
      // The popover is mounted twice (desktop sidebar + mobile strip) sharing
      // one open flag, so a click inside the VISIBLE instance is "outside" the
      // hidden one's ref. Closing on mousedown then unmounts the popover before
      // a button's click fires (swallowing Sign out / Sign in). Match any
      // popover instance via a shared marker instead of this instance's ref.
      const target = e.target as HTMLElement | null;
      if (
        !target?.closest?.(
          "[data-token-popover], [data-token-popover-trigger]",
        )
      ) {
        onClose();
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("mousedown", handler);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", handler);
      document.removeEventListener("keydown", onKey);
    };
  }, [open, onClose]);

  if (!open) return null;

  const save = async () => {
    setError(null);
    try {
      await api.login({ token: value });
    } catch (err) {
      setError(errorMessage(err));
      return;
    }
    setValue("");
    onSaved();
    onClose();
  };

  const signOut = async () => {
    await api.logout().catch(() => undefined);
    setValue("");
    onSignedOut();
    onClose();
  };

  const placementCls =
    placement === "below"
      ? "top-full right-0 mt-2 w-[min(calc(100vw-1.5rem),20rem)]"
      : "bottom-full left-0 mb-2 w-80";

  return (
    <div
      ref={ref}
      data-token-popover
      className={`absolute z-20 rounded-lg border border-zinc-700 bg-zinc-900 p-4 shadow-xl ${placementCls}`}
    >
      <label className="mb-1 block text-xs font-semibold text-zinc-400">
        Access token
      </label>
      <input
        type="password"
        className="w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-1.5 text-sm text-zinc-200 focus:border-sky-600 focus:outline-none"
        value={value}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") void save();
        }}
        placeholder="Paste your access token"
        autoFocus
      />
      {error ? <p className="mt-2 text-xs text-red-300">{error}</p> : null}
      <p className="mt-2 text-xs text-zinc-500">
        Used once to sign you in on this browser. Your token isn't saved here.
      </p>
      <div className="mt-3 flex justify-end gap-2">
        <Button variant="ghost" size="sm" onClick={() => void signOut()}>
          Sign out
        </Button>
        <Button variant="ghost" size="sm" onClick={onClose}>
          Cancel
        </Button>
        <Button variant="primary" size="sm" onClick={() => void save()}>
          Sign in
        </Button>
      </div>
    </div>
  );
}

function ConnectorChip({ status }: { status: ConnectorStatus | null }) {
  if (!status) {
    return (
      <span className="rounded-full bg-zinc-800 px-3 py-1 text-xs text-zinc-500">
        Connecting…
      </span>
    );
  }
  if (status.connected) {
    return (
      <span
        className="inline-flex items-center gap-1.5 rounded-full bg-emerald-500/10 px-3 py-1 text-xs text-emerald-300 ring-1 ring-inset ring-emerald-500/30"
        title="Google is connected for this account."
      >
        <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
        {status.service} connected
        {status.source ? ` · ${status.source}` : ""}
      </span>
    );
  }
  return (
    <span className="inline-flex items-center gap-1.5 rounded-full bg-red-500/10 px-3 py-1 text-xs text-red-300 ring-1 ring-inset ring-red-500/30">
      <span className="h-1.5 w-1.5 rounded-full bg-red-400" />
      {status.service} disconnected
    </span>
  );
}

function WhoAmIChip({ authEpoch }: { authEpoch: number }) {
  const [name, setName] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    api
      .whoami()
      .then((res) => {
        if (alive) setName(res.display_name);
      })
      .catch(() => {
        if (alive) setName(null);
      });
    return () => {
      alive = false;
    };
  }, [authEpoch]);
  if (name === null) return null;
  return (
    <span
      className="rounded-full bg-zinc-800 px-3 py-1 text-xs text-zinc-300 ring-1 ring-inset ring-zinc-700"
      title="Who your token signs you in as — actions are recorded under this name"
    >
      {name}
    </span>
  );
}

const SECTION_GROUPS: readonly SectionGroup[] = [
  "Work",
  "Records",
  "Automation",
  "System",
];

export default function App() {
  const [tab, setTab] = useState<Tab>("home");
  const [ruleSeed, setRuleSeed] = useState<EmailTriageRule | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [settingsSection, setSettingsSection] = useState<SettingsSectionId | null>(null);
  const [unauthorized, setUnauthorized] = useState(false);
  const [connector, setConnector] = useState<ConnectorStatus | null>(null);
  const [authEpoch, setAuthEpoch] = useState(0);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [helpOpen, setHelpOpen] = useState(false);
  const [helpInitialTopicId, setHelpInitialTopicId] = useState(GETTING_AROUND_TOPIC_ID);
  const [debugEnabled, setDebugEnabled] = useState(false);
  const [focusedDiagnosticId, setFocusedDiagnosticId] = useState<string | null>(null);
  const [focusedQueueItemId, setFocusedQueueItemId] = useState<string | null>(null);
  const [focusedTaskId, setFocusedTaskId] = useState<string | null>(null);
  const [focusedInboxId, setFocusedInboxId] = useState<string | null>(null);
  const [focusedInventoryId, setFocusedInventoryId] = useState<string | null>(null);
  const [focusedAccountingId, setFocusedAccountingId] = useState<string | null>(null);
  const [outputComposerOpen, setOutputComposerOpen] = useState(false);
  // Client brand + enabled slices from the overlay identity, via /readyz.
  const [brandName, setBrandName] = useState("BusinessOS");
  const [enabledSlices, setEnabledSlices] = useState<string[] | null>(null);
  const [visibleSlices, setVisibleSlices] = useState<string[] | null>(null);
  const [visibilityProbeFailed, setVisibilityProbeFailed] = useState(false);
  const [autoProduceEnabled, setAutoProduceEnabled] = useState(false);
  const [aiTriageEnabled, setAiTriageEnabled] = useState(false);
  const [agentLaunchEnabled, setAgentLaunchEnabled] = useState(false);

  useEffect(() => {
    void (async () => {
      try {
        const ready = await api.readyz();
        if (ready.display_name) setBrandName(ready.display_name);
        setEnabledSlices(ready.enabled_slices ?? []);
        setAutoProduceEnabled(ready.auto_produce_enabled ?? false);
        setAiTriageEnabled(ready.ai_triage_enabled ?? false);
        setAgentLaunchEnabled(ready.agent_launch_enabled ?? false);
      } catch {
        // Keep the shell usable if the optional identity probe fails.
        setEnabledSlices([]);
        setAutoProduceEnabled(false);
        setAiTriageEnabled(false);
        setAgentLaunchEnabled(false);
      }
    })();
  }, []);

  useEffect(() => {
    let alive = true;
    setVisibleSlices(null);
    setVisibilityProbeFailed(false);
    api
      .sessionVisibility()
      .then((visibility) => {
        if (!alive) return;
        setVisibleSlices(visibility.visible_slices ?? []);
        setVisibilityProbeFailed(false);
      })
      .catch((err) => {
        if (isUnauthorized(err)) setUnauthorized(true);
        if (!alive) return;
        setVisibleSlices(null);
        setVisibilityProbeFailed(!isUnauthorized(err));
      });
    return () => {
      alive = false;
    };
  }, [authEpoch]);

  useEffect(() => {
    document.title = brandName;
  }, [brandName]);

  const onUnauthorized = useCallback(() => setUnauthorized(true), []);

  const loadConnector = useCallback(async () => {
    try {
      const status = await api.connectorStatus();
      setConnector(status);
    } catch (err) {
      if (isUnauthorized(err)) setUnauthorized(true);
      setConnector(null);
    }
  }, []);

  useEffect(() => {
    void loadConnector();
  }, [loadConnector, authEpoch]);

  useEffect(() => {
    let alive = true;
    api
      .debugDiagnostics()
      .then(() => {
        if (alive) setDebugEnabled(true);
      })
      .catch((err) => {
        if (isUnauthorized(err)) setUnauthorized(true);
        if (alive) setDebugEnabled(false);
      });
    return () => {
      alive = false;
    };
  }, [authEpoch]);

  const onTokenSaved = useCallback(() => {
    setUnauthorized(false);
    setAuthEpoch((n) => n + 1);
  }, []);

  const onSignedOut = useCallback(() => {
    setUnauthorized(true);
    setAuthEpoch((n) => n + 1);
  }, []);

  const openHelp = useCallback((topicId = GETTING_AROUND_TOPIC_ID) => {
    setHelpInitialTopicId(topicId);
    setHelpOpen(true);
  }, []);

  // Global keyboard shortcuts: ⌘K/Ctrl+K palette, ? help.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "k") {
        e.preventDefault();
        setPaletteOpen((o) => !o);
        return;
      }
      if (e.key === "?" && !e.metaKey && !e.ctrlKey && !e.altKey) {
        const target = e.target as HTMLElement | null;
        if (
          target instanceof HTMLInputElement ||
          target instanceof HTMLTextAreaElement ||
          target instanceof HTMLSelectElement ||
          (target?.isContentEditable ?? false)
        ) {
          return;
        }
        openHelp();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [openHelp]);

  // enabledSlices is null until readyz returns. Until then, only tab surfaces
  // without a backing slice are available; that prevents disabled slice views
  // from mounting and firing 404ing API calls during startup. Once loaded, an
  // empty list means "all enabled" for compatibility with the backend overlay
  // posture.
  const operatorVisibleSlices = visibleSlices ?? (visibilityProbeFailed ? enabledSlices : null);
  const releaseNotesEnabled =
    operatorVisibleSlices !== null &&
    (operatorVisibleSlices.length === 0 || operatorVisibleSlices.includes("release_notes"));
  const sliceVisible = (sliceId: string) =>
    operatorVisibleSlices !== null &&
    (operatorVisibleSlices.length === 0 || operatorVisibleSlices.includes(sliceId));
  const outputComposerKinds: OutputKind[] = [
    ...(sliceVisible("email_drafts") ? (["email_draft_reply"] as const) : []),
    ...(sliceVisible("follow_up_tasks") ? (["follow_up_task"] as const) : []),
  ];
  const outputComposerEnabled =
    sliceVisible("operator_notes") &&
    sliceVisible("work_queue") &&
    outputComposerKinds.length > 0;

  const tabEnabled = useCallback(
    (t: Tab) => {
      const slice = sliceForTab(t);
      return (
        slice === undefined ||
        (operatorVisibleSlices !== null &&
          (operatorVisibleSlices.length === 0 || operatorVisibleSlices.includes(slice)))
      );
    },
    [operatorVisibleSlices],
  );

  const navGroups = useMemo(
    () => {
      const sections = NAV_SECTIONS.filter((section) =>
        sectionEnabled(section, operatorVisibleSlices),
      );
      return SECTION_GROUPS.map((group) => {
        const items = sections
          .filter((section) => section.group === group)
          .map((section) => ({
            tab: section.tab,
            label: section.label,
          }));
        if (debugEnabled && group === "System") {
          const usersIndex = items.findIndex((item) => item.tab === "users");
          const debugItem = { tab: "debug" as Tab, label: "Debug" };
          if (usersIndex === -1) {
            items.push(debugItem);
          } else {
            items.splice(usersIndex, 0, debugItem);
          }
        }
        return { label: group, items };
      }).filter((group) => group.items.length > 0);
    },
    [debugEnabled, operatorVisibleSlices],
  );

  const helpTopics = useMemo(
    () => buildHelpTopics({ enabledSlices: operatorVisibleSlices }),
    [operatorVisibleSlices],
  );

  // If the active tab gets gated out (e.g. landing default isn't enabled),
  // fall back to the first visible tab so we never render a 404 view.
  useEffect(() => {
    if (operatorVisibleSlices === null) return;
    if (!tabEnabled(tab)) {
      const firstVisible = navGroups[0]?.items[0]?.tab;
      if (firstVisible) setTab(firstVisible);
    }
  }, [operatorVisibleSlices, tabEnabled, tab, navGroups]);

  const navCommands: TargetedAppCommand[] = NAV_SECTIONS.map((section) => ({
    id: `nav-${section.tab}`,
    group: "Navigation",
    label: `Go to ${section.label}`,
    keywords: section.commandKeywords,
    run: () => setTab(section.tab),
  }));

  const debugCommands: TargetedAppCommand[] = debugEnabled
    ? [
        {
          id: "nav-debug",
          group: "Navigation",
          label: "Go to Debug",
          keywords: "debug diagnostics errors llm",
          run: () => setTab("debug"),
        },
      ]
    : [];

  const actionCommands: TargetedAppCommand[] = [
    ...(outputComposerEnabled
      ? [
          {
            id: "create-output",
            group: "Actions" as const,
            label: "Create output",
            keywords: "create compose email task blank manual ai",
            run: () => setOutputComposerOpen(true),
          },
        ]
      : []),
    {
      id: "refresh",
      group: "Actions",
      label: "Refresh current view",
      keywords: "reload refresh sync",
      run: () => dispatchAppCommand("refresh"),
    },
    {
      id: "open-help",
      group: "Actions",
      label: "Open Help",
      keywords: "help shortcuts support docs",
      shortcut: "?",
      run: () => openHelp(),
    },
    {
      id: "queue-log-note",
      group: "Actions",
      label: "Log note",
      keywords: "log note call walk-in queue",
      targetTab: "queue",
      run: () => {
        setTab("queue");
        dispatchAppCommand("queue.log-note");
      },
    },
    {
      id: "rules-new",
      group: "Actions",
      label: "New rule",
      keywords: "create rule triage",
      targetTab: "rules",
      run: () => {
        setTab("rules");
        dispatchAppCommand("rules.new");
      },
    },
    {
      id: "categories-new",
      group: "Actions",
      label: "New category",
      keywords: "create category classify",
      targetTab: "categories",
      run: () => {
        setTab("categories");
        dispatchAppCommand("categories.new");
      },
    },
    {
      id: "inventory-sync",
      group: "Actions",
      label: "Sync inventory now",
      keywords: "sync refresh stockforge inventory",
      targetTab: "inventory",
      run: () => {
        setTab("inventory");
        dispatchAppCommand("inventory.sync");
      },
    },
    {
      id: "accounting-sync",
      group: "Actions",
      label: "Sync accounting now",
      keywords: "sync refresh quickbooks accounting",
      targetTab: "accounting",
      run: () => {
        setTab("accounting");
        dispatchAppCommand("accounting.sync");
      },
    },
  ];

  const allCommands: TargetedAppCommand[] = [
    ...navCommands,
    ...debugCommands,
    ...actionCommands,
    ...articleBackedHelpTopics(helpTopics).map((topic) => ({
      id: `help-${topic.id}`,
      group: "Actions" as const,
      label: `Help: ${topic.title}`,
      keywords: topic.keywords.join(" "),
      run: () => openHelp(topic.id),
    })),
  ];

  const commands: AppCommand[] = allCommands.filter((command) => {
    const target = "targetTab" in command ? command.targetTab : undefined;
    if (target !== undefined) return tabEnabled(target);
    if (command.id.startsWith("nav-")) {
      return tabEnabled(command.id.slice(4) as Tab);
    }
    return true;
  });

  const navItemCls = (t: Tab) =>
    `w-full rounded-md px-3 py-1.5 text-left text-sm font-medium transition focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70 ${
      tab === t
        ? "bg-zinc-800 text-zinc-100"
        : "text-zinc-400 hover:bg-zinc-900 hover:text-zinc-200"
    }`;
  const pageTintClass = PAGE_TINT_BY_TAB[tab] ?? "";

  return (
    <div className="flex h-screen overflow-hidden">
      <CommandPalette
        open={paletteOpen}
        onClose={() => setPaletteOpen(false)}
        commands={commands}
      />
      {outputComposerOpen ? (
        <OutputComposer
          availableKinds={outputComposerKinds}
          onClose={() => setOutputComposerOpen(false)}
          onUnauthorized={onUnauthorized}
          onCreated={(itemId) => {
            setOutputComposerOpen(false);
            setFocusedQueueItemId(itemId);
            setTab("queue");
          }}
        />
      ) : null}
      <HelpDrawer
        open={helpOpen}
        onClose={() => setHelpOpen(false)}
        enabledSlices={enabledSlices}
        initialTopicId={helpInitialTopicId}
      />
      {/* Left sidebar — hidden on small screens, replaced by top nav strip */}
      <aside className="hidden lg:flex w-56 flex-none flex-col border-r border-zinc-800 bg-zinc-950">
        <div className="flex-1 overflow-y-auto px-3 py-4">
          <h1 className="mb-4 px-3 text-base font-bold tracking-tight text-zinc-100">
            {brandName}
          </h1>
          {outputComposerEnabled ? (
            <Button
              variant="primary"
              size="sm"
              className="mb-4 w-full justify-center"
              onClick={() => setOutputComposerOpen(true)}
            >
              + Create output
            </Button>
          ) : null}
          <nav className="flex flex-col gap-4">
            {navGroups.map((group) => (
              <div key={group.label}>
                <div className="mb-1 px-3 text-xs font-semibold uppercase tracking-wide text-zinc-500">
                  {group.label}
                </div>
                <div className="flex flex-col gap-0.5">
                  {group.items.map(({ tab: t, label }) => (
                    <button
                      key={t}
                      className={navItemCls(t)}
                      aria-current={tab === t ? "page" : undefined}
                      onClick={() => {
                        if (t === "settings") setSettingsSection(null);
                        setTab(t);
                      }}
                    >
                      {label}
                    </button>
                  ))}
                </div>
              </div>
            ))}
          </nav>
        </div>

        {/* Sidebar footer */}
        <div className="border-t border-zinc-800 px-3 py-3 flex flex-col gap-2">
          <WhoAmIChip authEpoch={authEpoch} />
          <ConnectorChip status={connector} />
          <div className="relative">
            <Button
              variant="secondary"
              size="sm"
              className="w-full justify-start"
              onClick={() => setSettingsOpen((o) => !o)}
              title="Operator token / account access"
              data-token-popover-trigger
            >
              Account
            </Button>
            <TokenPopover
              open={settingsOpen}
              onClose={() => setSettingsOpen(false)}
              onSaved={onTokenSaved}
              onSignedOut={onSignedOut}
            />
          </div>
          <Button
            variant="secondary"
            size="sm"
            className="w-full justify-start"
            onClick={() => openHelp()}
            title="Open help"
          >
            Help
          </Button>
          <ThemeToggle />
          <button
            onClick={() => setPaletteOpen(true)}
            className="w-full rounded-md px-3 py-1 text-left text-xs text-zinc-500 hover:text-zinc-400 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70"
            title={`${isMac ? "⌘K" : "Ctrl+K"} — command palette · ? — shortcuts`}
          >
            {isMac ? "⌘K" : "Ctrl+K"} — commands · ? — shortcuts
          </button>
        </div>
      </aside>

      {/* Small-screen top nav strip */}
      <div className="fixed inset-x-0 top-0 z-10 border-b border-zinc-800 bg-zinc-950/95 backdrop-blur lg:hidden">
        <div className="flex min-w-0 items-center gap-2 px-3 pt-2">
          <span className="min-w-0 flex-1 truncate text-sm font-bold text-zinc-100">
            {brandName}
          </span>
          <ThemeToggle compact />
          <Button
            variant="secondary"
            size="sm"
            className="flex-none"
            onClick={() => openHelp()}
            title="Open help"
          >
            Help
          </Button>
          <div className="relative flex-none">
            <Button
              variant="secondary"
              size="sm"
              onClick={() => setSettingsOpen((o) => !o)}
              title="Operator token / account access"
              data-token-popover-trigger
            >
              Account
            </Button>
            <TokenPopover
              open={settingsOpen}
              onClose={() => setSettingsOpen(false)}
              onSaved={onTokenSaved}
              onSignedOut={onSignedOut}
              placement="below"
            />
          </div>
        </div>
        <nav aria-label="Primary" className="flex gap-1 overflow-x-auto px-3 pb-2 pt-1.5">
          {outputComposerEnabled ? (
            <Button
              variant="primary"
              size="sm"
              className="flex-none"
              onClick={() => setOutputComposerOpen(true)}
            >
              + Output
            </Button>
          ) : null}
          {navGroups.flatMap((g) => g.items).map(({ tab: t, label }) => (
            <button
              key={t}
              onClick={() => {
                if (t === "settings") setSettingsSection(null);
                setTab(t);
              }}
              aria-current={tab === t ? "page" : undefined}
              className={`flex-none rounded-md px-2.5 py-1 text-xs font-medium transition focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70 ${
                tab === t
                  ? "bg-zinc-800 text-zinc-100"
                  : "text-zinc-400 hover:bg-zinc-900 hover:text-zinc-200"
              }`}
            >
              {label}
            </button>
          ))}
        </nav>
      </div>

      {/* Main content */}
      <div className="flex min-w-0 flex-1 flex-col overflow-hidden pt-[5.25rem] lg:pt-0">
        <main
          className={`flex-1 overflow-y-auto px-6 py-6 ${pageTintClass}`}
        >
          <div className="max-w-screen-xl">
            {unauthorized ? (
              <div className="mx-auto mt-16 max-w-md rounded-lg border border-zinc-800 bg-zinc-900/60 p-6 text-center">
                <h2 className="text-lg font-semibold text-zinc-100">
                  Operator token required
                </h2>
                <p className="mt-2 text-sm text-zinc-400">
                  Your browser session is missing or expired. Enter a valid token
                  to continue.
                </p>
                <UnauthorizedTokenForm onSaved={onTokenSaved} />
              </div>
            ) : (
              <CategoriesProvider key={authEpoch} onUnauthorized={onUnauthorized}>
                <ConnectorBanner status={connector} />
                {releaseNotesEnabled ? (
                  <ReleaseBanner onUnauthorized={onUnauthorized} />
                ) : null}
                {operatorVisibleSlices === null && sliceForTab(tab) !== undefined ? (
                  <div className="text-sm text-zinc-500">Loading…</div>
                ) : tab === "home" ? (
                  <Home
                    brandName={brandName}
                    onUnauthorized={onUnauthorized}
                    onNavigate={(target) => {
                      if (target.external_url) {
                        window.open(target.external_url, "_blank", "noopener,noreferrer");
                        if (!target.view) return;
                      }
                      if (target.view === "settings") {
                        setSettingsSection(
                          isSettingsSectionId(target.focus_id) ? target.focus_id : null,
                        );
                        setTab("settings");
                        return;
                      }
                      if (target.view === "debug") {
                        setFocusedDiagnosticId(target.focus_id ?? null);
                      }
                      if (target.view === "queue") {
                        setFocusedQueueItemId(target.focus_id ?? null);
                      }
                      if (!target.external_url && target.view === "tasks") {
                        setFocusedTaskId(target.focus_id ?? null);
                      }
                      if (!target.external_url && target.view === "inbox") {
                        setFocusedInboxId(target.focus_id ?? null);
                      }
                      if (target.view === "inventory") {
                        setFocusedInventoryId(target.focus_id ?? null);
                      }
                      if (!target.external_url && target.view === "accounting") {
                        setFocusedAccountingId(target.focus_id ?? null);
                      }
                      if (target.view) setTab(target.view);
                    }}
                  />
                ) : tab === "inbox" ? (
                  <Inbox
                    onUnauthorized={onUnauthorized}
                    helpTopicId={helpTopicIdForTab(helpTopics, "inbox")}
                    onOpenHelpTopic={openHelp}
                    onCreateRule={(seed) => {
                      setRuleSeed(seed);
                      setTab("rules");
                    }}
                    focusMessageId={focusedInboxId}
                    onFocusMessageConsumed={() => setFocusedInboxId(null)}
                  />
                ) : tab === "queue" ? (
                  <Queue
                    onUnauthorized={onUnauthorized}
                    helpTopicId={helpTopicIdForTab(helpTopics, "queue")}
                    onOpenHelpTopic={openHelp}
                    onOpenTasks={() => setTab("tasks")}
                    debugEnabled={debugEnabled}
                    agentLaunchEnabled={agentLaunchEnabled}
                    focusItemId={focusedQueueItemId}
                    onFocusItemConsumed={() => setFocusedQueueItemId(null)}
                    onOpenDebug={(diagnosticId) => {
                      setFocusedDiagnosticId(diagnosticId ?? null);
                      setTab("debug");
                    }}
                    onCreateOutput={
                      outputComposerEnabled
                        ? () => setOutputComposerOpen(true)
                        : undefined
                    }
                  />
                ) : tab === "tasks" ? (
                  <Tasks
                    onUnauthorized={onUnauthorized}
                    helpTopicId={helpTopicIdForTab(helpTopics, "tasks")}
                    onOpenHelpTopic={openHelp}
                    agentLaunchEnabled={agentLaunchEnabled}
                    focusTaskId={focusedTaskId}
                    onFocusTaskConsumed={() => setFocusedTaskId(null)}
                  />
                ) : tab === "calls" ? (
                  <CallInputs onUnauthorized={onUnauthorized} />
                ) : tab === "leads" ? (
                  <Leads
                    onUnauthorized={onUnauthorized}
                    onOpenQueue={(itemId) => {
                      setFocusedQueueItemId(itemId);
                      setTab("queue");
                    }}
                  />
                ) : tab === "plans" ? (
                  <ContentPlans
                    onUnauthorized={onUnauthorized}
                    helpTopicId={helpTopicIdForTab(helpTopics, "plans")}
                    onOpenHelpTopic={openHelp}
                    onOpenQueue={(itemId) => {
                      setFocusedQueueItemId(itemId);
                      setTab("queue");
                    }}
                  />
                ) : tab === "social" ? (
                  <SocialPublishing
                    onUnauthorized={onUnauthorized}
                    helpTopicId={helpTopicIdForTab(helpTopics, "social")}
                    onOpenHelpTopic={openHelp}
                  />
                ) : tab === "inventory" ? (
                  <Inventory
                    onUnauthorized={onUnauthorized}
                    helpTopicId={helpTopicIdForTab(helpTopics, "inventory")}
                    onOpenHelpTopic={openHelp}
                    focusInventoryId={focusedInventoryId}
                    onFocusInventoryConsumed={() => setFocusedInventoryId(null)}
                  />
                ) : tab === "accounting" ? (
                  <Accounting
                    onUnauthorized={onUnauthorized}
                    helpTopicId={helpTopicIdForTab(helpTopics, "accounting")}
                    onOpenHelpTopic={openHelp}
                    tierSyncEnabled={
                      enabledSlices === null ||
                      enabledSlices.length === 0 ||
                      enabledSlices.includes("customer_tier_sync")
                    }
                    focusAccountingId={focusedAccountingId}
                    onFocusAccountingConsumed={() => setFocusedAccountingId(null)}
                  />
                ) : tab === "analytics" ? (
                  <WebAnalytics onUnauthorized={onUnauthorized} />
                ) : tab === "reports" ? (
                  <Reports
                    onUnauthorized={onUnauthorized}
                    helpTopicId={helpTopicIdForTab(helpTopics, "reports")}
                    onOpenHelpTopic={openHelp}
                  />
                ) : tab === "settings" ? (
                  <Settings
                    onUnauthorized={onUnauthorized}
                    enabledSlices={operatorVisibleSlices}
                    helpTopics={helpTopics}
                    onOpenHelpTopic={openHelp}
                    initialSection={settingsSection}
                    onConnectorChanged={loadConnector}
                    aiTriageEnabled={aiTriageEnabled}
                  />
                ) : tab === "usage" ? (
                  <Usage
                    onUnauthorized={onUnauthorized}
                    helpTopicId={helpTopicIdForTab(helpTopics, "usage")}
                    onOpenHelpTopic={openHelp}
                    debugEnabled={debugEnabled}
                    onOpenDebug={(diagnosticId) => {
                      setFocusedDiagnosticId(diagnosticId ?? null);
                      setTab("debug");
                    }}
                  />
                ) : tab === "debug" ? (
                  <Debug
                    onUnauthorized={onUnauthorized}
                    focusDiagnosticId={focusedDiagnosticId}
                  />
                ) : tab === "users" ? (
                  <Users
                    onUnauthorized={onUnauthorized}
                    helpTopicId={helpTopicIdForTab(helpTopics, "users")}
                    onOpenHelpTopic={openHelp}
                  />
                ) : tab === "rules" ? (
                  <Rules
                    onUnauthorized={onUnauthorized}
                    helpTopicId={helpTopicIdForTab(helpTopics, "rules")}
                    onOpenHelpTopic={openHelp}
                    seed={ruleSeed}
                    onSeedConsumed={() => setRuleSeed(null)}
                    aiTriageEnabled={aiTriageEnabled}
                  />
                ) : (
                  <Categories
                    onUnauthorized={onUnauthorized}
                    helpTopicId={helpTopicIdForTab(helpTopics, "categories")}
                    onOpenHelpTopic={openHelp}
                    autoProduceEnabled={autoProduceEnabled}
                    aiTriageEnabled={aiTriageEnabled}
                    agentLaunchEnabled={agentLaunchEnabled}
                  />
                )}
              </CategoriesProvider>
            )}
          </div>
        </main>
      </div>
    </div>
  );
}

function UnauthorizedTokenForm({ onSaved }: { onSaved: () => void }) {
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const save = async () => {
    setError(null);
    try {
      await api.login({ token: value });
    } catch (err) {
      setError(errorMessage(err));
      return;
    }
    setValue("");
    onSaved();
  };
  return (
    <>
      <div className="mt-4 flex gap-2">
        <input
          type="password"
          className="flex-1 rounded-md border border-zinc-700 bg-zinc-950 px-3 py-2 text-sm text-zinc-200 focus:border-sky-600 focus:outline-none"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void save();
          }}
          placeholder="Paste your access token"
          autoFocus
        />
        <Button variant="primary" size="md" onClick={() => void save()}>
          Sign in
        </Button>
      </div>
      {error ? <p className="mt-2 text-sm text-red-300">{error}</p> : null}
    </>
  );
}
