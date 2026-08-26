import { useEffect } from "react";

type UsePollingOptions = {
  enabled?: boolean;
  immediate?: boolean;
  intervalMs: number;
};

export function usePolling(
  callback: () => void | Promise<void>,
  { enabled = true, immediate = true, intervalMs }: UsePollingOptions,
) {
  useEffect(() => {
    if (!enabled) return;
    if (immediate) void callback();

    const id = setInterval(() => void callback(), intervalMs);
    return () => clearInterval(id);
  }, [callback, enabled, immediate, intervalMs]);
}
