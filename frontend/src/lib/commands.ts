import { useEffect, useRef } from "react";

// ---------------------------------------------------------------------------
// Platform helper (shared by CommandPalette and ShortcutHelp)
// ---------------------------------------------------------------------------

export const isMac = /Mac|iP/.test(navigator.platform);

// ---------------------------------------------------------------------------
// App-command bus
// ---------------------------------------------------------------------------

const registry = new Map<string, () => void>();
let pendingCommand: string | null = null;

/**
 * Dispatch a named command. If a handler is registered, call it immediately.
 * Otherwise store it as a pending command (replaces any previous pending).
 */
export function dispatchAppCommand(name: string): void {
  const handler = registry.get(name);
  if (handler) {
    handler();
  } else {
    pendingCommand = name;
  }
}

/**
 * React hook: register a handler for a named command.
 * On mount, if there is a matching pending command, it is consumed immediately.
 * The handler ref is kept fresh so stale closures never fire.
 */
export function useAppCommand(name: string, handler: () => void): void {
  const handlerRef = useRef(handler);
  handlerRef.current = handler;

  useEffect(() => {
    const stableHandler = () => handlerRef.current();
    registry.set(name, stableHandler);
    // Consume a pending command that arrived before this component mounted.
    if (pendingCommand === name) {
      pendingCommand = null;
      stableHandler();
    }
    return () => {
      registry.delete(name);
    };
  }, [name]);
}
