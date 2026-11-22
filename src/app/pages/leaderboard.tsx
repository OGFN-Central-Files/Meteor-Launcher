import { CSSProperties, useEffect, useState } from "react";
import { RefreshCw, Zap } from "lucide-react";
import { motion } from "framer-motion";

import { useTheme } from "../../hooks/useTheme";
import { themeAlpha } from "../../util/color";
import { useUserStore } from "../../stores/user";
import { api } from "../../lib/api";

type Row = { username: string; hype?: number; discordAvatar?: string | null };

export function Leaderboard() {
  const { current } = useTheme();
  const accent = current.accent;
  const myName = useUserStore((s) => s.displayName);

  const [entries, setEntries] = useState<Row[]>([]);
  const [loading, setLoading] = useState(true);
  const [hovered, setHovered] = useState<number | null>(null);

  const load = async () => {
    setLoading(true);
    const res = await api.getHypeLeaderboard(100);
    if (res.success && res.data?.leaderboard) setEntries(res.data.leaderboard as Row[]);
    setLoading(false);
  };

  useEffect(() => {
    load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <motion.div
      initial={{ y: 16, opacity: 0 }}
      animate={{ y: 0, opacity: 1 }}
      transition={{ duration: 0.2 }}
      className="relative h-full w-full overflow-y-auto overflow-x-hidden"
    >
      <div className="relative px-6 md:px-8 py-8 max-w-4xl">
        {/* header + refresh */}
        <div className="flex items-start justify-between gap-4 mb-6">
          <div>
            <h1 className="text-3xl md:text-4xl font-black tracking-tight text-white">Leaderboard</h1>
            <p className={`mt-1.5 text-sm font-medium ${current.text.secondary}`}>
              Top 100 players ranked by Hype.
            </p>
          </div>
          <button
            onClick={load}
            className={`flex items-center gap-2 rounded-lg px-3.5 py-2 text-[13px] font-semibold ${current.text.secondary} hover:text-white transition-colors cursor-pointer`}
          >
            <RefreshCw size={14} className={loading ? "animate-spin" : ""} />
            Refresh
          </button>
        </div>

        {loading ? (
          <div className="space-y-2">
            {Array.from({ length: 8 }).map((_, i) => (
              <div key={i} className={`h-14 rounded-xl border ${current.border} animate-pulse`} style={{ backgroundColor: themeAlpha(current.background.secondary, 0.4) }} />
            ))}
          </div>
        ) : entries.length === 0 ? (
          <div className={`rounded-xl border ${current.border} py-16 text-center`} style={{ backgroundColor: themeAlpha(current.background.secondary, 0.3) }}>
            <Zap size={26} style={{ color: accent }} className="mx-auto mb-3 opacity-60" />
            <p className={`text-[13px] ${current.text.secondary}`}>No hype recorded yet — jump into Arena to get on the board.</p>
          </div>
        ) : (
          <>
            {/* column header */}
            <div className={`flex items-center justify-between px-4 pb-2 mb-1 border-b ${current.border}`}>
              <div className="flex items-center gap-4">
                <span className={`text-[11px] font-bold uppercase tracking-wider ${current.text.secondary}`}>Rank</span>
                <span className={`text-[11px] font-bold uppercase tracking-wider ${current.text.secondary}`}>Player</span>
              </div>
              <span className={`text-[11px] font-bold uppercase tracking-wider ${current.text.secondary}`}>Hype</span>
            </div>

            <div className="space-y-1">
              {entries.map((e, index) => {
                const rank = index + 1;
                const isUser = !!myName && e.username.toLowerCase() === myName.toLowerCase();
                const isHovered = index === hovered;

                const rankBadge =
                  rank === 1 ? "bg-yellow-400/20 text-yellow-300"
                  : rank === 2 ? "bg-slate-300/20 text-slate-200"
                  : rank === 3 ? "bg-orange-400/20 text-orange-300"
                  : `${current.background.secondary} ${current.text.secondary}`;

                const rowStyle: CSSProperties = isUser
                  ? { borderColor: `${accent}70`, backgroundColor: `${accent}12` }
                  : { backgroundColor: isHovered ? themeAlpha(current.background.secondary, 0.6) : themeAlpha(current.background.secondary, 0.35) };

                return (
                  <motion.div
                    key={`${e.username}-${index}`}
                    initial={{ opacity: 0, y: 6 }}
                    animate={{ opacity: 1, y: 0 }}
                    transition={{ duration: 0.2, delay: Math.min(index, 18) * 0.02 }}
                    onMouseEnter={() => setHovered(index)}
                    onMouseLeave={() => setHovered(null)}
                    style={rowStyle}
                    className={`flex items-center justify-between rounded-xl border ${current.border} px-4 py-2.5 transition-colors duration-150`}
                  >
                    <div className="flex items-center gap-4 min-w-0">
                      <span className={`flex items-center justify-center w-7 h-7 rounded-lg text-xs font-bold shrink-0 ${rankBadge}`}>{rank}</span>
                      {e.discordAvatar ? (
                        <img src={e.discordAvatar} className="w-8 h-8 rounded-full shrink-0 object-cover border border-white/10" onError={(ev) => ((ev.currentTarget.style.display = "none"))} />
                      ) : (
                        <div className="w-8 h-8 rounded-full shrink-0 flex items-center justify-center text-xs font-bold" style={{ backgroundColor: `${accent}22`, color: accent }}>
                          {e.username.charAt(0).toUpperCase()}
                        </div>
                      )}
                      <p className={`font-semibold truncate ${isUser ? "text-white" : current.text.primary}`}>{e.username}</p>
                    </div>
                    <span className="font-bold flex items-center gap-1.5 shrink-0" style={{ color: accent }}>
                      <Zap size={15} strokeWidth={2.5} />
                      {(e.hype ?? 0).toLocaleString()}
                    </span>
                  </motion.div>
                );
              })}
            </div>
          </>
        )}
      </div>
    </motion.div>
  );
}
