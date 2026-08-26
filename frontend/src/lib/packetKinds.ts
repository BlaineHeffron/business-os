import { useEffect, useState } from "react";
import type { PacketKindRecord } from "../types/generated/PacketKindRecord";
import { api } from "./api";

// Module-level cache: the catalog is platform-defined and static for a
// session, so one fetch serves every view that mounts the hook.
let cache: PacketKindRecord[] | null = null;
let inflight: Promise<PacketKindRecord[]> | null = null;

function fetchKinds(): Promise<PacketKindRecord[]> {
  if (cache !== null) return Promise.resolve(cache);
  inflight ??= api
    .packetKinds()
    .then((res) => {
      cache = res.kinds;
      return res.kinds;
    })
    .catch((err: unknown) => {
      inflight = null; // allow retry on next mount
      throw err;
    });
  return inflight;
}

/**
 * Platform packet-kind catalog (GET /api/work-queue/packet-kinds), fetched
 * once per session. Returns [] until loaded; on failure stays [] and callers
 * degrade to raw kind ids.
 */
export function usePacketKinds(): PacketKindRecord[] {
  const [kinds, setKinds] = useState<PacketKindRecord[]>(cache ?? []);

  useEffect(() => {
    let alive = true;
    fetchKinds()
      .then((k) => {
        if (alive) setKinds(k);
      })
      .catch(() => {
        // Degrade silently: chips render raw ids without titles.
      });
    return () => {
      alive = false;
    };
  }, []);

  return kinds;
}
