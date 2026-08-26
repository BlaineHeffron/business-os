import { useEffect, useRef, useState } from "react";
import type { AppCommand } from "../lib/commandTypes";

interface CommandPaletteProps {
  open: boolean;
  onClose: () => void;
  commands: AppCommand[];
}

export default function CommandPalette({
  open,
  onClose,
  commands,
}: CommandPaletteProps) {
  const [query, setQuery] = useState("");
  const [selectedIdx, setSelectedIdx] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const itemRefs = useRef<(HTMLButtonElement | null)[]>([]);

  // Reset state whenever the palette opens.
  useEffect(() => {
    if (open) {
      setQuery("");
      setSelectedIdx(0);
    }
  }, [open]);

  // Focus input on open.
  useEffect(() => {
    if (open) {
      // Defer one tick so the element is visible.
      const id = setTimeout(() => inputRef.current?.focus(), 0);
      return () => clearTimeout(id);
    }
  }, [open]);

  const filtered = commands.filter((cmd) => {
    if (!query.trim()) return true;
    const q = query.toLowerCase();
    return (
      cmd.label.toLowerCase().includes(q) ||
      (cmd.keywords ?? "").toLowerCase().includes(q)
    );
  });

  // Group into Navigation and Actions.
  const navCommands = filtered.filter((c) => c.group === "Navigation");
  const actionCommands = filtered.filter((c) => c.group === "Actions");
  const ordered = [...navCommands, ...actionCommands];

  // Clamp selection when filtered list changes.
  const clampedIdx = Math.min(selectedIdx, Math.max(ordered.length - 1, 0));

  // Scroll selected item into view.
  useEffect(() => {
    itemRefs.current[clampedIdx]?.scrollIntoView({ block: "nearest" });
  }, [clampedIdx]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      } else if (e.key === "ArrowDown") {
        e.preventDefault();
        setSelectedIdx((i) =>
          ordered.length === 0 ? 0 : (i + 1) % ordered.length,
        );
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        setSelectedIdx((i) =>
          ordered.length === 0
            ? 0
            : (i - 1 + ordered.length) % ordered.length,
        );
      } else if (e.key === "Enter") {
        e.preventDefault();
        const cmd = ordered[clampedIdx];
        if (cmd) {
          cmd.run();
          onClose();
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose, ordered, clampedIdx]);

  if (!open) return null;

  const runCmd = (cmd: AppCommand) => {
    cmd.run();
    onClose();
  };

  // Rebuild per render so indices stay correct.
  itemRefs.current = [];

  const renderGroup = (
    label: string,
    items: AppCommand[],
    baseIdx: number,
  ) => {
    if (items.length === 0) return null;
    return (
      <div key={label}>
        <div className="px-3 pb-1 pt-2 text-xs font-semibold uppercase tracking-wide text-zinc-500">
          {label}
        </div>
        {items.map((cmd, i) => {
          const idx = baseIdx + i;
          const selected = idx === clampedIdx;
          return (
            <button
              key={cmd.id}
              ref={(el) => {
                itemRefs.current[idx] = el;
              }}
              className={`flex w-full items-center justify-between rounded px-3 py-1.5 text-left text-sm transition ${
                selected
                  ? "bg-zinc-800 text-zinc-100"
                  : "text-zinc-300 hover:bg-zinc-800/60 hover:text-zinc-100"
              }`}
              onMouseEnter={() => setSelectedIdx(idx)}
              onClick={() => runCmd(cmd)}
            >
              <span>{cmd.label}</span>
              {cmd.shortcut ? (
                <span className="ml-4 shrink-0 text-xs text-zinc-500">
                  {cmd.shortcut}
                </span>
              ) : null}
            </button>
          );
        })}
      </div>
    );
  };

  return (
    <div
      className="fixed inset-0 z-50 bg-black/50"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="mx-auto mt-[15vh] w-full max-w-md rounded-lg border border-zinc-700 bg-zinc-900 shadow-xl">
        <div className="border-b border-zinc-800 px-3 py-2">
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setSelectedIdx(0);
            }}
            placeholder="Type a command…"
            className="w-full bg-transparent text-sm text-zinc-100 placeholder:text-zinc-500 focus:outline-none"
            aria-label="Command palette search"
          />
        </div>
        <div
          ref={listRef}
          className="max-h-72 overflow-y-auto py-1"
        >
          {ordered.length === 0 ? (
            <p className="px-3 py-2 text-xs text-zinc-400">
              No matching commands.
            </p>
          ) : (
            <>
              {renderGroup("Navigation", navCommands, 0)}
              {renderGroup("Actions", actionCommands, navCommands.length)}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
