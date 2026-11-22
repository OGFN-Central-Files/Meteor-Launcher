import { useEffect, useMemo, useState } from "react";
import { motion } from "framer-motion";
import { Users } from "lucide-react";

import { useTheme } from "../../hooks/useTheme";
import { themeAlpha, hexToRgba } from "../../util/color";
import { api, GameServerEntry } from "../../lib/api";

const STALE_MS = 45_000;
const POLL_MS = 5_000;

const JOIN = "#34d399"; // joinable = green
const INGAME = "#a855f7"; // in game = accent purple
const WARN = "#f59e0b";
const OFF = "#7c8598";

function isLive(s: GameServerEntry) {
  return Date.now() - s.lastSeen <= STALE_MS;
}

// Region -> the full label used in the filter + row pills ("Europe", "NA-East", "OCE").
function regionFull(r: string): string {
  const s = (r || "").toUpperCase();
  if (s.startsWith("NAE") || s.startsWith("NA")) return "NA-East";
  if (s.startsWith("EU")) return "Europe";
  if (s.startsWith("OCE") || s.startsWith("AU") || s === "NZ") return "OCE";
  return s || "??";
}

// Playlist -> team size pill ("Solo" / "Duos" / "Squads").
function modeLabel(pl: string): string {
  const s = (pl || "").toLowerCase();
  if (s.includes("duo")) return "Duos";
  if (s.includes("squad")) return "Squads";
  return "Solo";
}

type Row = { label: string; color: string; text: string };
function rowState(s: GameServerEntry): Row {
  switch ((s.status || "").toLowerCase()) {
    case "available": return { label: "Joinable", color: JOIN, text: "Joinable" };
    case "in_game": return { label: "In game", color: INGAME, text: "In game" };
    case "starting": return { label: "Booting", color: WARN, text: "Booting" };
    case "ended": return { label: "Restarting", color: WARN, text: "Restarting" };
    default: return { label: s.status || "—", color: OFF, text: s.status || "—" };
  }
}

export function ServerStatus() {
  const { current } = useTheme();
  const border = current.border;
  const textSecondary = current.text.secondary;

  const [servers, setServers] = useState<GameServerEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [filter, setFilter] = useState<string>("All");

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      const list = await api.getServerList();
      if (cancelled) return;
      const real = list.success && list.data?.servers ? list.data.servers : [];
      // DEV-only preview data so the rows are visible without a live gameserver (never ships).
      if (!real.length && import.meta.env.DEV) {
        const now = Date.now();
        setServers([
          { serverId: "eu-01", name: "eu-01", status: "available", region: "EU", playlist: "Playlist_DefaultDuo", playerCount: 0, maxPlayers: 100, lastSeen: now } as GameServerEntry,
          { serverId: "nae-01", name: "nae-01", status: "in_game", region: "NAE", playlist: "Playlist_DefaultDuo", playerCount: 3, maxPlayers: 100, lastSeen: now } as GameServerEntry,
          { serverId: "eu-02", name: "eu-02", status: "in_game", region: "EU", playlist: "Playlist_DefaultDuo", playerCount: 3, maxPlayers: 100, lastSeen: now } as GameServerEntry,
        ]);
      } else {
        setServers(real);
      }
      setLoading(false);
    };
    load();
    const id = setInterval(load, POLL_MS);
    return () => { cancelled = true; clearInterval(id); };
  }, []);

  const { live, onlineCount, inGamePlayers, regions } = useMemo(() => {
    const lv = servers.filter(isLive);
    const inGame = lv.filter((s) => (s.status || "").toLowerCase() === "in_game");
    const players = inGame.reduce((n, s) => n + (s.playerCount || 0), 0);
    const regs = Array.from(new Set(lv.map((s) => regionFull(s.region))));
    return { live: lv, onlineCount: lv.length, inGamePlayers: players, regions: regs };
  }, [servers]);

  const filtered = useMemo(
    () => live.filter((s) => filter === "All" || regionFull(s.region) === filter),
    [live, filter],
  );

  const joinable = filtered.filter((s) => (s.status || "").toLowerCase() === "available");
  const inGameList = filtered.filter((s) => (s.status || "").toLowerCase() === "in_game");
  const other = filtered.filter((s) => {
    const st = (s.status || "").toLowerCase();
    return st !== "available" && st !== "in_game";
  });

  const filters = ["All", ...regions];

  const ServerRow = ({ s, i }: { s: GameServerEntry; i: number }) => {
    const st = rowState(s);
    const max = s.maxPlayers && s.maxPlayers > 0 ? s.maxPlayers : 100;
    const count = s.playerCount || 0;
    const pct = Math.max(1.5, Math.min(100, (count / max) * 100));
    return (
      <motion.div
        layout
        initial={{ opacity: 0, y: 8 }}
        animate={{ opacity: 1, y: 0 }}
        transition={{ delay: Math.min(i, 10) * 0.03 }}
        className={`rounded-xl border ${border} px-4 py-3`}
        style={{ backgroundColor: themeAlpha(current.background.secondary, 0.45) }}
      >
        <div className="flex items-center justify-between gap-3">
          <div className="flex items-center gap-2">
            <span className="text-[12px] font-bold text-white px-2.5 py-1 rounded-md" style={{ backgroundColor: themeAlpha(current.background.secondary, 0.9) }}>
              {modeLabel(s.playlist)}
            </span>
            <span className="text-[12px] font-bold text-white px-2.5 py-1 rounded-md" style={{ backgroundColor: themeAlpha(current.background.secondary, 0.9) }}>
              {regionFull(s.region)}
            </span>
          </div>
          <div className="flex items-center gap-3 shrink-0">
            <span className="text-[13px] font-semibold" style={{ color: st.color }}>{st.text}</span>
            <span className="flex items-center gap-1.5 text-[13px] font-bold text-white tabular-nums">
              <Users size={13} className="opacity-70" />
              {count}<span className={textSecondary}>/{max}</span>
            </span>
          </div>
        </div>
        <div className="h-1.5 rounded-full overflow-hidden mt-3" style={{ backgroundColor: hexToRgba("#ffffff", 0.07) }}>
          <motion.div className="h-full rounded-full" style={{ background: st.color }} initial={false} animate={{ width: `${pct}%` }} transition={{ duration: 0.5 }} />
        </div>
      </motion.div>
    );
  };

  const Section = ({ title, items }: { title: string; items: GameServerEntry[] }) =>
    items.length ? (
      <div className="mb-6">
        <h2 className={`text-[11px] font-bold uppercase tracking-widest mb-2.5 ${textSecondary}`}>{title}</h2>
        <div className="space-y-2.5">
          {items.map((s, i) => <ServerRow key={s.serverId} s={s} i={i} />)}
        </div>
      </div>
    ) : null;

  return (
    <motion.div initial={{ y: 16, opacity: 0 }} animate={{ y: 0, opacity: 1 }} transition={{ duration: 0.2 }} className="relative h-full w-full overflow-y-auto overflow-x-hidden">
      <div className="relative px-6 md:px-8 py-8 max-w-4xl">
        {/* header */}
        <h1 className="text-3xl md:text-4xl font-black tracking-tight text-white">Servers</h1>
        <p className={`mt-1.5 text-sm font-medium ${textSecondary}`}>
          {loading ? "Checking servers…" : `${onlineCount} online, ${inGamePlayers} in game right now.`}
        </p>

        {/* region filter */}
        <div className="flex items-center gap-2 flex-wrap mt-5 mb-6">
          {filters.map((f) => {
            const active = filter === f;
            return (
              <button
                key={f}
                onClick={() => setFilter(f)}
                className="rounded-lg px-3.5 py-1.5 text-[12px] font-semibold transition-colors cursor-pointer"
                style={{
                  backgroundColor: active ? themeAlpha(current.background.secondary, 0.95) : "transparent",
                  color: active ? "#ffffff" : textSecondary,
                  border: `1px solid ${active ? hexToRgba("#ffffff", 0.12) : "transparent"}`,
                }}
              >
                {f}
              </button>
            );
          })}
        </div>

        {loading ? (
          <div className="space-y-2.5">
            {Array.from({ length: 3 }).map((_, i) => (
              <div key={i} className={`h-16 rounded-xl border ${border} animate-pulse`} style={{ backgroundColor: themeAlpha(current.background.secondary, 0.35) }} />
            ))}
          </div>
        ) : filtered.length === 0 ? (
          <div className={`rounded-xl border ${border} py-14 text-center`} style={{ backgroundColor: themeAlpha(current.background.secondary, 0.3) }}>
            <p className={`text-[13px] ${textSecondary}`}>
              {servers.length === 0 || live.length === 0
                ? "No servers are online right now — they'll appear the moment they come up."
                : `No ${filter} servers online right now.`}
            </p>
          </div>
        ) : (
          <>
            <Section title="Joinable" items={joinable} />
            <Section title="In Game" items={inGameList} />
            <Section title="Other" items={other} />
          </>
        )}
      </div>
    </motion.div>
  );
}
