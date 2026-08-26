import { useEffect, useRef, useState, type ReactNode } from "react";
import { Button, StatusBadge } from "../ui";

export type OutputComposerTab = { id: string; label: string };

/** Shared Linear-style output workspace. Queue/context and blank/manual modes
 * intentionally share the same frame: typed editor dominates, governed
 * context is useful but collapsible, and the action bar stays visible. */
export default function OutputComposerShell({
  title,
  mode,
  tabs,
  activeTab,
  onSelectTab,
  tabsDisabled = false,
  contextTitle,
  context,
  children,
  footer,
  onClose,
  closeLabel = "Close",
}: {
  title: string;
  mode: "queue" | "blank";
  tabs: OutputComposerTab[];
  activeTab: string;
  onSelectTab: (id: string) => void;
  tabsDisabled?: boolean;
  contextTitle: string;
  context: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  onClose: () => void;
  closeLabel?: string;
}) {
  const [contextOpen, setContextOpen] = useState(true);
  const closeRef = useRef<HTMLButtonElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const opener = document.activeElement as HTMLElement | null;
    closeRef.current?.focus();
    return () => opener?.focus();
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
        return;
      }
      if (event.key !== "Tab") return;

      const dialog = dialogRef.current;
      if (!dialog) return;
      const focusable = Array.from(
        dialog.querySelectorAll<HTMLElement>(
          'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
        ),
      ).filter((element) => element.getAttribute("aria-hidden") !== "true");
      const first = focusable[0];
      const last = focusable.at(-1);
      if (!first || !last) {
        event.preventDefault();
        return;
      }

      const active = document.activeElement;
      if (event.shiftKey && (active === first || !dialog.contains(active))) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && (active === last || !dialog.contains(active))) {
        event.preventDefault();
        first.focus();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  return (
    <div className="fixed inset-0 z-40 bg-black/70 p-2 sm:p-5">
      <div
        ref={dialogRef}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        className="mx-auto flex h-full max-w-screen-2xl flex-col overflow-hidden rounded-lg border border-zinc-700 bg-zinc-950 shadow-2xl"
      >
        <header className="flex flex-wrap items-center gap-3 border-b border-zinc-800 px-4 py-3">
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2">
              <h2 className="truncate text-sm font-semibold text-zinc-100">{title}</h2>
              <StatusBadge tone={mode === "blank" ? "info" : "neutral"}>
                {mode === "blank" ? "blank output" : "queue context"}
              </StatusBadge>
            </div>
            {tabs.length > 0 ? (
              <div role="tablist" className="mt-2 flex flex-wrap gap-1">
                {tabs.map((tab) => (
                  <button
                    key={tab.id}
                    role="tab"
                    aria-selected={tab.id === activeTab}
                    disabled={tabsDisabled}
                    onClick={() => onSelectTab(tab.id)}
                    className={`rounded-md border px-2.5 py-1 text-xs font-medium transition focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-sky-500/70 disabled:cursor-not-allowed disabled:opacity-50 ${
                      tab.id === activeTab
                        ? "border-sky-500/50 bg-sky-500/10 text-sky-200"
                        : "border-transparent text-zinc-400 hover:border-zinc-700 hover:bg-zinc-800/50 hover:text-zinc-200"
                    }`}
                  >
                    {tab.label}
                  </button>
                ))}
              </div>
            ) : null}
          </div>
          <Button
            variant="secondary"
            size="sm"
            onClick={() => setContextOpen((open) => !open)}
            aria-expanded={contextOpen}
          >
            {contextOpen ? "Hide context" : "Show context"}
          </Button>
          <Button ref={closeRef} variant="secondary" size="sm" onClick={onClose}>
            {closeLabel}
          </Button>
        </header>

        <div
          className={`grid min-h-0 flex-1 ${
            contextOpen
              ? "grid-cols-1 lg:grid-cols-[minmax(17rem,22rem)_minmax(0,1fr)]"
              : "grid-cols-1"
          }`}
        >
          {contextOpen ? (
            <aside className="min-h-0 overflow-y-auto border-b border-zinc-800 bg-zinc-950/80 lg:border-b-0 lg:border-r">
              <div className="sticky top-0 border-b border-zinc-800 bg-zinc-950/95 px-4 py-2 text-xs font-semibold uppercase tracking-wide text-zinc-400 backdrop-blur">
                {contextTitle}
              </div>
              {context}
            </aside>
          ) : null}
          <section className="min-h-0 overflow-y-auto bg-zinc-950">{children}</section>
        </div>

        {footer ? (
          <footer className="border-t border-zinc-800 bg-zinc-950/95 px-4 py-3 backdrop-blur">
            {footer}
          </footer>
        ) : null}
      </div>
    </div>
  );
}
