// A length of time as its two largest units: 1781023 -> "20d 14h", 192 -> "3m 12s".
// A count of seconds alone stops being readable past a few minutes.
export function fmtDuration(seconds) {
  const d = Math.floor(seconds / 86400)
  const h = Math.floor((seconds % 86400) / 3600)
  const m = Math.floor((seconds % 3600) / 60)
  const s = Math.floor(seconds % 60)
  if (d) return `${d}d ${h}h`
  if (h) return `${h}h ${m}m`
  if (m) return `${m}m ${s}s`
  return `${s}s`
}
