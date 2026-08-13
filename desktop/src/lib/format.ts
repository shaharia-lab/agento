/* ============================================================================
   Formatters. Shared so every surface renders the same number the same way —
   the analytics page and the session row must never disagree about a total.
   ========================================================================== */

/** 1234 → "1.2K", 1_200_000 → "1.2M". Used for token counts. */
export function compactNumber(n: number | undefined | null): string {
  if (n === undefined || n === null || !isFinite(n)) return "—";
  const abs = Math.abs(n);
  if (abs < 1000) return String(Math.round(n));
  if (abs < 1_000_000) return trim(n / 1000) + "K";
  if (abs < 1_000_000_000) return trim(n / 1_000_000) + "M";
  return trim(n / 1_000_000_000) + "B";
}

function trim(v: number): string {
  // One decimal, but never a trailing ".0" — "1M" reads better than "1.0M".
  const s = v.toFixed(1);
  return s.endsWith(".0") ? s.slice(0, -2) : s;
}

export function integer(n: number | undefined | null): string {
  if (n === undefined || n === null || !isFinite(n)) return "—";
  return Math.round(n).toLocaleString();
}

/**
 * Money. Sub-cent values keep four decimals rather than rounding to $0.00 —
 * a per-session cost is often genuinely fractions of a cent, and showing zero
 * would read as "free".
 */
export function usd(n: number | undefined | null): string {
  if (n === undefined || n === null || !isFinite(n)) return "—";
  if (n === 0) return "$0.00";
  if (Math.abs(n) < 0.01) return `$${n.toFixed(4)}`;
  if (Math.abs(n) >= 10_000) {
    return `$${n.toLocaleString(undefined, { maximumFractionDigits: 0 })}`;
  }
  return `$${n.toLocaleString(undefined, {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })}`;
}

export function percent(n: number | undefined | null, digits = 1): string {
  if (n === undefined || n === null || !isFinite(n)) return "—";
  return `${n.toFixed(digits)}%`;
}

/** Milliseconds → "4m 12s". Compact, for table cells and meta lines. */
export function duration(ms: number | undefined | null): string {
  if (ms === undefined || ms === null || !isFinite(ms) || ms <= 0) return "—";
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ${m % 60}m`;
  return `${Math.floor(h / 24)}d ${h % 24}h`;
}

/** "3 m ago", "2 h ago", "Yesterday", "12 Aug". */
export function relativeTime(iso: string | undefined | null): string {
  if (!iso) return "—";
  const then = new Date(iso).getTime();
  if (!isFinite(then)) return "—";

  const diff = Date.now() - then;
  if (diff < 0) return upcoming(-diff);

  const min = Math.floor(diff / 60_000);
  if (min < 1) return "just now";
  if (min < 60) return `${min} m ago`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr} h ago`;
  const day = Math.floor(hr / 24);
  if (day === 1) return "Yesterday";
  if (day < 7) return `${day} d ago`;
  return shortDate(iso);
}

function upcoming(ms: number): string {
  const min = Math.round(ms / 60_000);
  if (min < 1) return "now";
  if (min < 60) return `in ${min} m`;
  const hr = Math.round(min / 60);
  if (hr < 24) return `in ${hr} h`;
  return `in ${Math.round(hr / 24)} d`;
}

export function shortDate(iso: string | undefined | null): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (!isFinite(d.getTime())) return "—";
  return d.toLocaleDateString(undefined, { day: "numeric", month: "short" });
}

export function clockTime(iso: string | undefined | null): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (!isFinite(d.getTime())) return "—";
  return d.toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function dateTime(iso: string | undefined | null): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (!isFinite(d.getTime())) return "—";
  return d.toLocaleString(undefined, {
    day: "numeric",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/**
 * Group items under "Today" / "Yesterday" / "Previous 7 Days" / "Older",
 * preserving input order within each group.
 */
export function groupByRecency<T>(
  items: T[],
  getDate: (item: T) => string
): [string, T[]][] {
  const buckets = new Map<string, T[]>();
  const order = ["Today", "Yesterday", "Previous 7 Days", "Older"];

  for (const item of items) {
    const label = recencyLabel(getDate(item));
    const list = buckets.get(label) ?? [];
    list.push(item);
    buckets.set(label, list);
  }

  return order
    .filter((label) => buckets.has(label))
    .map((label) => [label, buckets.get(label)!]);
}

function recencyLabel(iso: string): string {
  const d = new Date(iso);
  if (!isFinite(d.getTime())) return "Older";

  const startOfToday = new Date();
  startOfToday.setHours(0, 0, 0, 0);

  if (d.getTime() >= startOfToday.getTime()) return "Today";
  if (d.getTime() >= startOfToday.getTime() - 86_400_000) return "Yesterday";
  if (d.getTime() >= startOfToday.getTime() - 7 * 86_400_000)
    return "Previous 7 Days";
  return "Older";
}

/** "~/Projects/x" for paths under the home directory. */
export function tildePath(path: string | undefined | null): string {
  if (!path) return "—";
  const home = path.match(/^\/(?:home|Users)\/[^/]+/);
  return home ? "~" + path.slice(home[0].length) : path;
}

/** Initials for an avatar tile: "Apartment Scout" → "AS". */
export function initials(name: string | undefined | null): string {
  if (!name) return "?";
  const parts = name.trim().split(/[\s_-]+/).filter(Boolean);
  if (!parts.length) return "?";
  if (parts.length === 1) return parts[0].slice(0, 2).toUpperCase();
  return (parts[0][0] + parts[1][0]).toUpperCase();
}

/** Stable colour pick so the same agent always gets the same tile colour. */
const TONES = ["accent", "green", "amber", "purple", "teal", "red"] as const;
export type Tone = (typeof TONES)[number];

export function toneFor(key: string | undefined | null): Tone {
  if (!key) return "accent";
  let hash = 0;
  for (let i = 0; i < key.length; i++) {
    hash = (hash * 31 + key.charCodeAt(i)) >>> 0;
  }
  return TONES[hash % TONES.length];
}
