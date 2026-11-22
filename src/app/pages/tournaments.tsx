import { CSSProperties, useEffect, useMemo, useState } from "react";
import { motion, AnimatePresence } from "framer-motion";
import { Swords, Zap, Trophy, Clock, Gift, X } from "lucide-react";
import { useTheme } from "../../hooks/useTheme";
import { hexToRgba, themeAlpha } from "../../util/color";
import { api, Tournament } from "../../lib/api";

const POLL_MS = 5000;

const STATUS = {
  live: { label: "LIVE", color: "#34d399" },
  upcoming: { label: "UPCOMING", color: "#f59e0b" },
  ended: { label: "ENDED", color: "#7c8598" },
} as const;

const YEAR_MS = 365 * 86400 * 1000;
const pad = (n: number) => String(n).padStart(2, "0");

// "22.8.26  |  20:00 - 22:30", guarding the year-9999 "no end" sentinel.
function fmtDateTime(t: Tournament): string {
  const start = Date.parse(t.startTime);
  const end = Date.parse(t.endTime);
  if (Number.isNaN(start)) return t.status === "live" ? "Live now" : "";
  const d = new Date(start);
  const dateStr = `${d.getDate()}.${d.getMonth() + 1}.${String(d.getFullYear()).slice(2)}`;
  let timeStr = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  if (!Number.isNaN(end) && end > start && end - start < YEAR_MS) {
    const e = new Date(end);
    timeStr += ` - ${pad(e.getHours())}:${pad(e.getMinutes())}`;
  }
  return `${dateStr}  |  ${timeStr}`;
}

function endsIn(t: Tournament): string | null {
  const end = Date.parse(t.endTime);
  const now = Date.now();
  if (t.status !== "live") return null;
  if (Number.isNaN(end) || end <= now || end - now >= YEAR_MS) return "Live now";
  const s = Math.floor((end - now) / 1000);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  return h > 0 ? `Ends in ${h}h ${m}m` : `Ends in ${m}m`;
}

export function Tournaments() {
  const colors = useTheme();
  const accent = colors.current.accent;
  const border = colors.current.border;
  const textPrimary = colors.current.text.primary;
  const textSecondary = colors.current.text.secondary;

  const [data, setData] = useState<{ live: Tournament[]; upcoming: Tournament[]; past: Tournament[] }>({
    live: [],
    upcoming: [],
    past: [],
  });
  const [loading, setLoading] = useState(true);
  const [tab, setTab] = useState<"tournaments" | "leaderboard">("tournaments");
  const [openId, setOpenId] = useState<string | null>(null);
  const [boardId, setBoardId] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const pull = async () => {
      const res = await api.getTournaments();
      if (cancelled) return;
      if (res.success && res.data) {
        setData({ live: res.data.live || [], upcoming: res.data.upcoming || [], past: res.data.past || [] });
      }
      setLoading(false);
    };
    pull();
    const id = setInterval(pull, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, []);

  const all = useMemo(() => [...data.live, ...data.upcoming, ...data.past], [data]);
  const boardCups = useMemo(() => [...data.live, ...data.past], [data]);
  const opened = useMemo(() => all.find((t) => t.id === openId) || null, [all, openId]);
  const board = useMemo(() => {
    if (boardId) { const f = boardCups.find((t) => t.id === boardId); if (f) return f; }
    return boardCups[0] || null;
  }, [boardId, boardCups]);

  // Close the detail modal on Escape.
  useEffect(() => {
    if (!openId) return;
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") setOpenId(null); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [openId]);

  const rankBadge = (rank: number) =>
    rank === 1
      ? "bg-yellow-400/20 text-yellow-300"
      : rank === 2
        ? "bg-slate-300/20 text-slate-200"
        : rank === 3
          ? "bg-orange-400/20 text-orange-300"
          : `${colors.current.background.secondary} ${textSecondary}`;

  const StatusPill = ({ status, small }: { status: Tournament["status"]; small?: boolean }) => {
    const s = STATUS[status];
    return (
      <span
        className={`inline-flex items-center gap-1.5 font-bold tracking-widest uppercase rounded-full ${small ? "text-[9px] px-2 py-0.5" : "text-[10px] px-2.5 py-1"}`}
        style={{ backgroundColor: hexToRgba(s.color, 0.16), color: s.color }}
      >
        {status === "live" && (
          <motion.span
            className="h-1.5 w-1.5 rounded-full"
            style={{ backgroundColor: s.color }}
            animate={{ opacity: [1, 0.3, 1] }}
            transition={{ repeat: Infinity, duration: 1.4 }}
          />
        )}
        {s.label}
      </span>
    );
  };

  // Tall poster card (art + name + date/time), click to open details.
  const Poster = ({ t, index }: { t: Tournament; index: number }) => {
    const art = t.image || t.tileImage;
    return (
      <motion.button
        initial={{ opacity: 0, y: 12 }}
        animate={{ opacity: 1, y: 0 }}
        transition={{ duration: 0.25, delay: Math.min(index, 12) * 0.04 }}
        whileHover={{ y: -4 }}
        onClick={() => setOpenId(t.id)}
        className={`group relative rounded-2xl overflow-hidden border ${border} cursor-pointer text-left`}
        style={{ aspectRatio: "3 / 4", backgroundColor: themeAlpha(colors.current.background.secondary, 0.5) }}
      >
        {art ? (
          <img
            src={art}
            className="absolute inset-0 w-full h-full object-cover transition-transform duration-300 group-hover:scale-105"
            onError={(e) => ((e.currentTarget.style.display = "none"))}
          />
        ) : (
          <div className="absolute inset-0 flex items-center justify-center">
            <Swords size={30} style={{ color: accent }} className="opacity-50" />
          </div>
        )}

        {/* darken bottom for text legibility */}
        <div className="absolute inset-0" style={{ background: "linear-gradient(180deg, rgba(0,0,0,0) 45%, rgba(0,0,0,0.85) 100%)" }} />

        {/* status pill (live/upcoming only) top-left */}
        {t.status !== "ended" && (
          <div className="absolute top-2.5 left-2.5">
            <StatusPill status={t.status} small />
          </div>
        )}

        {/* selection ring on hover */}
        <div
          className="absolute inset-0 rounded-2xl opacity-0 group-hover:opacity-100 transition-opacity duration-200"
          style={{ boxShadow: `inset 0 0 0 2px ${accent}` }}
        />

        <div className="absolute inset-x-0 bottom-0 p-3">
          <h3 className="text-white font-black text-base tracking-tight truncate drop-shadow">{t.name}</h3>
          <p className="text-white/70 text-[11px] font-medium mt-0.5">{fmtDateTime(t)}</p>
        </div>
      </motion.button>
    );
  };

  const PosterGrid = ({ items }: { items: Tournament[] }) => (
    <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5 gap-4">
      {items.map((t, i) => (
        <Poster key={t.id} t={t} index={i} />
      ))}
    </div>
  );

  const Section = ({ title, items }: { title: string; items: Tournament[] }) =>
    items.length ? (
      <div className="mb-8">
        <h2 className={`text-sm font-bold uppercase tracking-widest mb-3 ${textSecondary}`}>{title}</h2>
        <PosterGrid items={items} />
      </div>
    ) : null;

  const Standings = ({ t, max = 100 }: { t: Tournament; max?: number }) => {
    if (!t.leaderboard.length) {
      return (
        <div className={`flex flex-col items-center justify-center gap-2 py-12 ${textSecondary}`}>
          <Trophy size={24} style={{ color: accent }} />
          <p className="text-sm">No scores recorded yet for this cup.</p>
        </div>
      );
    }
    return (
      <div className="divide-y divide-white/5">
        {t.leaderboard.slice(0, max).map((row, index) => {
          const rank = index + 1;
          const rowStyle: CSSProperties = {
            backgroundColor: index % 2 ? themeAlpha(colors.current.background.secondary, 0.25) : "transparent",
          };
          return (
            <div key={`${row.username}-${index}`} className="flex items-center justify-between px-5 py-2.5" style={rowStyle}>
              <div className="flex items-center gap-4 min-w-0">
                <span className={`flex items-center justify-center w-7 h-7 rounded-lg text-xs font-bold shrink-0 ${rankBadge(rank)}`}>{rank}</span>
                {row.discordAvatar ? (
                  <img src={row.discordAvatar} className="w-8 h-8 rounded-full shrink-0 object-cover border border-white/10" onError={(e) => ((e.currentTarget.style.display = "none"))} />
                ) : (
                  <div className="w-8 h-8 rounded-full shrink-0 flex items-center justify-center text-xs font-bold" style={{ backgroundColor: `${accent}22`, color: accent }}>
                    {row.username.charAt(0).toUpperCase()}
                  </div>
                )}
                <div className="min-w-0">
                  <p className={`font-semibold truncate ${textPrimary}`}>{row.username}</p>
                  <p className={`text-xs truncate ${textSecondary}`}>{row.wins} wins · {row.elims} elims · {row.matches} games</p>
                </div>
              </div>
              <span className="font-bold flex items-center gap-1.5 shrink-0" style={{ color: accent }}>
                <Zap size={15} strokeWidth={2.5} />
                {row.points.toLocaleString()}
              </span>
            </div>
          );
        })}
      </div>
    );
  };

  const Toggle = () => (
    <div className={`flex items-center gap-1 rounded-xl p-1 border ${border} shrink-0`} style={{ backgroundColor: themeAlpha(colors.current.background.secondary, 0.5) }}>
      {(["tournaments", "leaderboard"] as const).map((k) => {
        const active = tab === k;
        return (
          <button
            key={k}
            onClick={() => setTab(k)}
            className={`px-3.5 py-1.5 rounded-lg text-xs font-bold capitalize cursor-pointer transition-colors duration-150 ${active ? "" : textSecondary}`}
            style={{ backgroundColor: active ? accent : "transparent", color: active ? "#0b0f17" : undefined }}
          >
            {k}
          </button>
        );
      })}
    </div>
  );

  if (loading) {
    return (
      <div className="h-full overflow-hidden px-8 pt-8">
        <div className="h-9 w-56 rounded-lg bg-white/5 animate-pulse mb-3" />
        <div className="h-4 w-80 rounded-lg bg-white/5 animate-pulse mb-6" />
        <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5 gap-4">
          {Array.from({ length: 5 }).map((_, i) => (
            <div key={i} className={`rounded-2xl border ${border} animate-pulse`} style={{ aspectRatio: "3 / 4", backgroundColor: themeAlpha(colors.current.background.secondary, 0.4) }} />
          ))}
        </div>
      </div>
    );
  }

  const nothing = !data.live.length && !data.upcoming.length && !data.past.length;

  return (
    <motion.div initial={{ y: 16, opacity: 0 }} animate={{ y: 0, opacity: 1 }} transition={{ duration: 0.2 }} className="relative h-full w-full overflow-y-auto overflow-x-hidden">
      <div className="pointer-events-none absolute -top-24 -right-24 w-96 h-96 rounded-full blur-3xl opacity-[0.12]" style={{ background: accent }} />

      <div className="relative px-6 md:px-8 py-8 max-w-6xl mx-auto">
        <div className="flex items-start justify-between gap-4 mb-7 flex-wrap">
          <div>
            <h1 className="text-3xl md:text-4xl font-black tracking-tight bg-linear-to-r from-white to-zinc-400 bg-clip-text text-transparent">Tournaments</h1>
            <p className={`mt-2 text-sm font-medium ${textSecondary}`}>Live, upcoming and past events.</p>
          </div>
          <Toggle />
        </div>

        {nothing ? (
          <div className={`flex flex-col items-center justify-center gap-3 rounded-2xl border ${border} backdrop-blur-xl py-20`} style={{ backgroundColor: themeAlpha(colors.current.background.secondary, 0.4) }}>
            <Swords size={30} style={{ color: accent }} />
            <p className={`text-sm ${textSecondary}`}>No tournaments right now — check back soon.</p>
          </div>
        ) : tab === "tournaments" ? (
          <>
            <Section title="Live Now" items={data.live} />
            <Section title="Upcoming" items={data.upcoming} />
            <Section title="Past Events" items={data.past} />
          </>
        ) : (
          // Leaderboard tab: standings of the live cup (or most recent past), with a selector.
          <div>
            {boardCups.length > 1 && (
              <div className="flex items-center gap-2 flex-wrap mb-4">
                {boardCups.map((t) => {
                  const active = board?.id === t.id;
                  return (
                    <button
                      key={t.id}
                      onClick={() => setBoardId(t.id)}
                      className={`px-3 py-1.5 rounded-lg text-xs font-semibold cursor-pointer border ${border} transition-colors duration-150 ${active ? "" : textSecondary}`}
                      style={{ backgroundColor: active ? hexToRgba(accent, 0.16) : themeAlpha(colors.current.background.secondary, 0.4), color: active ? accent : undefined }}
                    >
                      {t.name}
                    </button>
                  );
                })}
              </div>
            )}
            {board ? (
              <div className={`rounded-2xl border ${border} backdrop-blur-xl overflow-hidden`} style={{ backgroundColor: themeAlpha(colors.current.background.secondary, 0.4) }}>
                <div className={`flex items-center justify-between px-5 py-3 border-b ${border}`}>
                  <span className={`text-xs font-bold uppercase tracking-wider ${textSecondary}`}>Player</span>
                  <span className={`text-xs font-bold uppercase tracking-wider ${textSecondary}`}>Points</span>
                </div>
                <Standings t={board} />
              </div>
            ) : (
              <div className={`text-center py-16 text-sm ${textSecondary}`}>No standings yet.</div>
            )}
          </div>
        )}
      </div>

      {/* Detail modal */}
      <AnimatePresence>
        {opened && (
          <motion.div
            className="fixed inset-0 z-50 flex items-center justify-center p-4 md:p-8"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            onClick={() => setOpenId(null)}
            style={{ backgroundColor: "rgba(0,0,0,0.72)", backdropFilter: "blur(6px)" }}
          >
            <motion.div
              initial={{ scale: 0.96, y: 12, opacity: 0 }}
              animate={{ scale: 1, y: 0, opacity: 1 }}
              exit={{ scale: 0.96, y: 12, opacity: 0 }}
              transition={{ duration: 0.2 }}
              onClick={(e) => e.stopPropagation()}
              className={`relative w-full max-w-2xl max-h-[86vh] rounded-2xl border ${border} overflow-hidden flex flex-col`}
              style={{ backgroundColor: themeAlpha(colors.current.background.primary, 0.98), boxShadow: `0 40px 120px -30px rgba(0,0,0,0.8), 0 0 0 1px ${hexToRgba(accent, 0.1)}` }}
            >
              {/* Banner */}
              <div className="relative h-44 shrink-0 bg-black/50">
                {(opened.tileImage || opened.image) && (
                  <img src={opened.tileImage || opened.image} className="absolute inset-0 w-full h-full object-cover" onError={(e) => ((e.currentTarget.style.display = "none"))} />
                )}
                <div className="absolute inset-0" style={{ background: `linear-gradient(180deg, rgba(0,0,0,0.1) 0%, ${themeAlpha(colors.current.background.primary, 0.98)} 100%)` }} />
                <button
                  onClick={() => setOpenId(null)}
                  className="absolute top-3 right-3 w-8 h-8 rounded-lg flex items-center justify-center text-white/80 hover:text-white cursor-pointer"
                  style={{ backgroundColor: "rgba(0,0,0,0.45)" }}
                  aria-label="Close"
                >
                  <X size={16} />
                </button>
                <div className="absolute inset-x-0 bottom-0 p-5">
                  <div className="flex items-center gap-2.5 mb-2">
                    <StatusPill status={opened.status} />
                    {endsIn(opened) && (
                      <span className="text-xs font-medium inline-flex items-center gap-1 text-white/80"><Clock size={12} /> {endsIn(opened)}</span>
                    )}
                  </div>
                  <h2 className="text-2xl font-black tracking-tight text-white drop-shadow">{opened.name}</h2>
                </div>
              </div>

              {/* Info + standings */}
              <div className="flex-1 min-h-0 overflow-y-auto">
                <div className="p-5 space-y-3">
                  <p className={`text-xs font-medium inline-flex items-center gap-1.5 ${textSecondary}`}>
                    <Clock size={13} /> {fmtDateTime(opened) || "TBA"}
                  </p>
                  {opened.description && <p className={`text-sm ${textSecondary}`}>{opened.description}</p>}
                  {opened.prize && (
                    <div className={`rounded-xl border ${border} p-3.5`} style={{ backgroundColor: themeAlpha(colors.current.background.secondary, 0.5) }}>
                      <p className="text-[10px] font-bold uppercase tracking-widest mb-1.5" style={{ color: accent }}>Prizes</p>
                      <p className="text-sm font-medium inline-flex items-start gap-2" style={{ color: accent }}>
                        <Gift size={15} className="mt-[2px] shrink-0" />
                        <span>{opened.prize}</span>
                      </p>
                    </div>
                  )}
                </div>

                {opened.status !== "upcoming" && (
                  <div>
                    <div className={`flex items-center justify-between px-5 py-3 border-y ${border}`} style={{ backgroundColor: themeAlpha(colors.current.background.secondary, 0.4) }}>
                      <span className={`text-xs font-bold uppercase tracking-wider ${textSecondary}`}>Standings</span>
                      <span className={`text-xs font-bold uppercase tracking-wider ${textSecondary}`}>Points</span>
                    </div>
                    <Standings t={opened} />
                  </div>
                )}
              </div>
            </motion.div>
          </motion.div>
        )}
      </AnimatePresence>
    </motion.div>
  );
}
