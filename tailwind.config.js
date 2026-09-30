/** @type {import('tailwindcss').Config} */
// Tokens from STYLE.md. Templates use these names, never raw hex.
module.exports = {
  content: ["./templates/**/*.html"],
  theme: {
    extend: {
      colors: {
        canvas: "#0b0b0f",
        surface: "#131318",
        raised: "#1c1c23",
        line: "rgb(255 255 255 / 0.08)",
        "line-strong": "rgb(255 255 255 / 0.16)",
        ink: "#f4f4f5",
        muted: "#a1a1aa",
        faint: "#71717a",
        live: "#4ade80",
        warn: "#fbbf24",
        danger: "#f87171",
        gold: "#f5c542",
      },
      fontFamily: {
        sans: ["Inter", "ui-sans-serif", "system-ui", "sans-serif"],
        mono: ["JetBrains Mono", "ui-monospace", "monospace"],
      },
    },
  },
  plugins: [],
};
