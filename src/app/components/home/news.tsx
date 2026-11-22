import { motion } from "framer-motion";

import { useTheme } from "../../../hooks/useTheme";
import { hexToRgba, themeAlpha } from "../../../util/color";

// News is intentionally a SINGLE clean image — no text, no carousel, and NOT
// backend-driven. We always show the bundled Meteor banner (public/news.webp);
// the backend is deliberately ignored here so its Season-X art can never
// overwrite the Meteor artwork.
const IMAGE = "/news.webp";

export function News() {
  const colors = useTheme();
  const accent = colors.current.accent;

  return (
    <motion.div
      initial={{ opacity: 0, y: 12 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: 0.4 }}
      className={`relative rounded-3xl border ${colors.current.border} overflow-hidden`}
      style={{
        backgroundColor: themeAlpha(colors.current.background.secondary, 0.4),
        boxShadow: `0 22px 60px -26px rgba(0,0,0,0.75), 0 0 0 1px ${hexToRgba(accent, 0.12)}`,
      }}
    >
      {/* soft accent glow behind the banner */}
      <div
        className="pointer-events-none absolute -top-24 -right-16 w-72 h-72 rounded-full blur-3xl opacity-25"
        style={{ background: accent }}
      />
      <motion.img
        src={IMAGE}
        alt="News"
        className="relative block w-full h-auto select-none"
        draggable={false}
        // Gentle, slow ken-burns so the banner is subtly alive but never crops the art.
        initial={{ scale: 1.015 }}
        animate={{ scale: [1.015, 1.045, 1.015] }}
        transition={{ duration: 18, repeat: Infinity, ease: "easeInOut" }}
        onError={(e) => {
          (e.currentTarget as HTMLImageElement).style.display = "none";
        }}
      />
    </motion.div>
  );
}
