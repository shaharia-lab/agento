import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Dropdown, Empty, Search } from "../../components/ui";
import { CopyButton } from "../../components/CopyButton";
import { Icon } from "../../lib/icons";
import { describeError } from "../../lib/hooks";
import { relativeTime } from "../../lib/format";
import {
  LEVELS,
  LOGS_AVAILABLE,
  exportLogs,
  formatBytes,
  logFiles,
  parseLog,
  pickSavePath,
  readLog,
  type LogEntry,
  type LogFile,
  type LogLevel,
} from "../../lib/logs";
import "../../styles/logs.css";

/** How much of a file the first read pulls back. */
const FIRST_READ = 512 * 1024;

/** What "Load more" multiplies that by, up to the whole file. */
const READ_STEP = 4;

/**
 * Most rows rendered at once.
 *
 * The session table is not virtualised and neither is this; the difference is
 * that a log has no natural size, so a cap is the thing standing between a
 * 5 MiB file and ~50k DOM nodes. The newest rows are the ones kept, and the
 * banner says so rather than letting the list quietly lie about what it holds.
 */
const RENDER_CAP = 4000;

/** How often a followed file is re-read. */
const FOLLOW_MS = 2000;

const LEVEL_LABEL: Record<LogLevel, string> = {
  ERROR: "Errors",
  WARN: "Warnings",
  INFO: "Info",
  DEBUG: "Debug",
  TRACE: "Trace",
};

/**
 * Settings → Logs.
 *
 * The file this reads is the one a user is asked to attach to a bug report:
 * `proxy.rs` writes an access line per `/api` request into it, failures at
 * warn and writes at info (#301). The pane exists so that "send me your logs"
 * is a button rather than a paragraph of platform-specific paths.
 *
 * Three things it is deliberately not: it does not stream (the log plugin has
 * no tail event, so following is a 2 s re-read of what was appended), it does
 * not virtualise (see `RENDER_CAP`), and it does not filter anything out of
 * the *export* — a redacted log is worth less than no log, and nothing in the
 * file is a secret by construction (`proxy.rs` logs no bodies, headers or
 * query strings).
 */
export function LogsPane() {
  const [files, setFiles] = useState<LogFile[]>([]);
  const [dir, setDir] = useState("");
  const [selected, setSelected] = useState<string>();
  const [entries, setEntries] = useState<LogEntry[]>([]);
  const [window_, setWindow] = useState(FIRST_READ);
  const [truncated, setTruncated] = useState(false);
  const [size, setSize] = useState(0);
  const [error, setError] = useState<string>();
  const [loading, setLoading] = useState(true);
  const [follow, setFollow] = useState(true);
  const [query, setQuery] = useState("");
  const [hidden, setHidden] = useState<Set<LogLevel>>(new Set());
  const [saved, setSaved] = useState<string>();
  // Bumped by every full read, so the jump to the newest line also happens on
  // a reload or a file switch — where the row count may not have changed and
  // the scroll effect would otherwise not fire.
  const [reads, setReads] = useState(0);

  // The offset a follow resumes from. A ref rather than state because the poll
  // reads it on every tick and must not re-arm the interval when it moves.
  const cursor = useRef(0);
  const body = useRef<HTMLDivElement>(null);
  // Whether the view is parked at the bottom. Following scrolls only when it
  // is, so reading back through history is not yanked away every two seconds.
  const atBottom = useRef(true);

  /* --- Loading ----------------------------------------------------------- */

  const load = useCallback(
    async (name: string | undefined, bytes: number) => {
      setLoading(true);
      try {
        const chunk = await readLog({ name, maxBytes: bytes });
        setEntries(parseLog(chunk.text));
        setTruncated(chunk.truncated);
        setSize(chunk.size);
        cursor.current = chunk.next;
        atBottom.current = true;
        setReads((n) => n + 1);
        setError(undefined);
      } catch (err) {
        setEntries([]);
        setError(describeError(err));
      } finally {
        setLoading(false);
      }
    },
    []
  );

  // The file list, once. It is also what decides whether the pane has anything
  // to show at all: no files means nothing has been logged yet.
  useEffect(() => {
    if (!LOGS_AVAILABLE) {
      setLoading(false);
      return;
    }
    let cancelled = false;
    logFiles()
      .then((index) => {
        if (cancelled) return;
        setDir(index.dir);
        setFiles(index.files);
        if (index.files.length === 0) setLoading(false);
      })
      .catch((err) => {
        if (cancelled) return;
        setError(describeError(err));
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Re-read whenever the chosen file or the window changes. `selected` being
  // undefined means the live file, which is what the shell picks by default.
  useEffect(() => {
    if (!LOGS_AVAILABLE || files.length === 0) return;
    void load(selected, window_);
  }, [selected, window_, files.length, load]);

  const isLive = useMemo(
    () => files.find((f) => f.name === selected)?.live ?? selected === undefined,
    [files, selected]
  );

  // Following an archive would poll a file nothing writes to, so it is only
  // ever armed on the live one.
  useEffect(() => {
    if (!LOGS_AVAILABLE || !follow || !isLive || error) return;
    const timer = window.setInterval(async () => {
      try {
        const chunk = await readLog({
          name: selected,
          maxBytes: window_,
          from: cursor.current,
        });
        cursor.current = chunk.next;
        setSize(chunk.size);
        if (chunk.reset) {
          // The file rotated underneath us: what we hold describes a file that
          // no longer exists.
          setEntries(parseLog(chunk.text));
          setTruncated(chunk.truncated);
        } else if (chunk.text) {
          // Parsed onto a copy of the existing list so a continuation line
          // still lands on the entry it belongs to.
          setEntries((prev) => parseLog(chunk.text, prev.slice()));
        }
      } catch {
        /* a transient read failure is not worth tearing the view down over */
      }
    }, FOLLOW_MS);
    return () => window.clearInterval(timer);
  }, [follow, isLive, selected, window_, error]);

  /* --- Filtering --------------------------------------------------------- */

  const counts = useMemo(() => {
    const out = { ERROR: 0, WARN: 0, INFO: 0, DEBUG: 0, TRACE: 0 };
    for (const e of entries) out[e.level]++;
    return out;
  }, [entries]);

  const filtered = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return entries.filter((e) => {
      if (hidden.has(e.level)) return false;
      if (!needle) return true;
      return (
        e.text.toLowerCase().includes(needle) ||
        e.target.toLowerCase().includes(needle) ||
        e.time.includes(needle)
      );
    });
  }, [entries, hidden, query]);

  const shown = filtered.length > RENDER_CAP ? filtered.slice(-RENDER_CAP) : filtered;

  // Stick to the bottom, but only from the bottom: reading back through
  // history must not be yanked away by the next poll.
  useEffect(() => {
    if (!atBottom.current) return;
    const el = body.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [shown.length, reads]);

  /* --- Actions ----------------------------------------------------------- */

  async function save() {
    setSaved(undefined);
    try {
      const stamp = new Date().toISOString().slice(0, 10);
      const dest = await pickSavePath(`agento-logs-${stamp}.log`);
      if (!dest) return;
      const bytes = await exportLogs(dest);
      setSaved(`Saved ${formatBytes(bytes)} to ${dest}`);
    } catch (err) {
      setError(describeError(err));
    }
  }

  function toggleLevel(level: LogLevel) {
    setHidden((prev) => {
      const next = new Set(prev);
      if (next.has(level)) next.delete(level);
      else next.add(level);
      return next;
    });
  }

  /* --- Render ------------------------------------------------------------ */

  if (!LOGS_AVAILABLE) {
    return (
      <Empty
        icon="terminal"
        title="Logs are a desktop feature"
        text="The log file belongs to the app itself, so it can only be read from the desktop window — not from a browser tab."
      />
    );
  }

  if (!loading && files.length === 0 && !error) {
    return (
      <Empty
        icon="terminal"
        title="Nothing logged yet"
        text={
          dir
            ? `This install has written no log file. It will appear in ${dir}.`
            : "This install has written no log file yet."
        }
      />
    );
  }

  const fileOptions = files.map((f) => ({
    value: f.name,
    label: `${f.live ? "Current" : archiveLabel(f.name)} — ${formatBytes(f.bytes)}`,
  }));

  return (
    <div className="logspane">
      <div className="logs__bar">
        <Dropdown
          small
          value={selected ?? files.find((f) => f.live)?.name ?? files[0]?.name ?? ""}
          options={fileOptions}
          onChange={(v) => {
            setWindow(FIRST_READ);
            setSelected(v);
          }}
          ariaLabel="Log file"
          style={{ minWidth: 190 }}
        />
        <Search value={query} onChange={setQuery} placeholder="Filter log" />
        <button
          className={`btn ${follow && isLive ? "btn--on" : ""}`}
          onClick={() => setFollow((f) => !f)}
          disabled={!isLive}
          title={
            isLive
              ? "Re-read the file every two seconds and stay at the newest line"
              : "Only the current file is still being written to"
          }
        >
          <Icon name={follow && isLive ? "pause" : "play"} size={13} />
          {follow && isLive ? "Following" : "Follow"}
        </button>
        <button className="btn" onClick={() => void load(selected, window_)}>
          <Icon name="refresh" size={13} />
          Reload
        </button>
        <button className="btn btn--primary" onClick={() => void save()}>
          <Icon name="download" size={13} />
          Save a copy…
        </button>
      </div>

      <div className="logs__bar logs__bar--levels">
        {LEVELS.map((level) => (
          <button
            key={level}
            className={`logchip logchip--${level.toLowerCase()} ${
              hidden.has(level) ? "logchip--off" : ""
            }`}
            onClick={() => toggleLevel(level)}
            title={
              hidden.has(level) ? `Show ${LEVEL_LABEL[level]}` : `Hide ${LEVEL_LABEL[level]}`
            }
          >
            <span className="logchip__dot" />
            {LEVEL_LABEL[level]}
            <span className="logchip__count tnum">{counts[level]}</span>
          </button>
        ))}
        <span className="logs__spacer" />
        <span className="logs__meta">
          {formatBytes(size)}
          {truncated && " · tail"}
        </span>
        <CopyButton
          text={() => shown.map(lineOf).join("\n")}
          title="Copy the visible lines"
          className="btn"
          size={13}
          label="Copy"
        />
      </div>

      {error && (
        <div className="msgline msgline--error">
          <span className="msgline__icon">
            <Icon name="alert" size={13} />
          </span>
          <span>{error}</span>
        </div>
      )}
      {saved && (
        <div className="msgline msgline--ok">
          <span className="msgline__icon">
            <Icon name="check" size={13} />
          </span>
          <span className="selectable">{saved}</span>
        </div>
      )}

      {truncated && (
        <div className="logs__notice">
          <span>
            Showing the last {formatBytes(window_)} of this file.
          </span>
          <button className="btn btn--sm" onClick={() => setWindow((w) => w * READ_STEP)}>
            Load more
          </button>
        </div>
      )}
      {filtered.length > shown.length && (
        <div className="logs__notice">
          Showing the newest {RENDER_CAP.toLocaleString()} of{" "}
          {filtered.length.toLocaleString()} matching lines. Filter to narrow it, or
          save a copy for the rest.
        </div>
      )}

      <div
        className="logs__body"
        ref={body}
        onScroll={(e) => {
          const el = e.currentTarget;
          atBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
        }}
      >
        {loading && entries.length === 0 ? (
          <div className="logs__idle">Reading the log…</div>
        ) : shown.length === 0 ? (
          <div className="logs__idle">
            {entries.length === 0 ? "This file is empty." : "No lines match."}
          </div>
        ) : (
          shown.map((e) => (
            <div key={e.id} className={`logline logline--${e.level.toLowerCase()}`}>
              <span className="logline__time tnum">{e.time.slice(11) || "—"}</span>
              <span className="logline__level">{e.level}</span>
              <span className="logline__target" title={e.target}>
                {shortTarget(e.target)}
              </span>
              <span className="logline__text selectable">{e.text}</span>
            </div>
          ))
        )}
      </div>

      <div className="logs__foot">
        <span className="logs__path selectable mono">{dir}</span>
        <CopyButton text={dir} title="Copy the log directory path" />
        <span className="logs__spacer" />
        <span className="logs__meta">
          {files.length} file{files.length === 1 ? "" : "s"}
          {files[0]?.modified_ms
            ? ` · updated ${relativeTime(new Date(files[0].modified_ms).toISOString())}`
            : ""}
        </span>
      </div>
    </div>
  );
}

/**
 * `Agento_2026-08-21_09-00-00.log` → `2026-08-21 09:00:00`.
 *
 * The rotator's own name format, read back: date and time joined by `_`, with
 * the clock's colons written as hyphens because a filename cannot hold them.
 * A name that does not match is shown as it is rather than mangled.
 */
function archiveLabel(name: string): string {
  const m = /_(\d{4}-\d{2}-\d{2})_(\d{2})-(\d{2})-(\d{2})\.log(\.bak)?$/.exec(name);
  if (!m) return name;
  return `${m[1]} ${m[2]}:${m[3]}:${m[4]}${m[5] ? " (bak)" : ""}`;
}

/**
 * The whole line, for the clipboard — what someone pastes into an issue, so it
 * is spelled the way the file spells it (target then level) rather than the
 * way the table renders it.
 */
function lineOf(e: LogEntry): string {
  return `[${e.time}][${e.target}][${e.level}] ${e.text}`;
}

/**
 * `agento_lib::native::chat::turn` is most of the row's width and none of its
 * information; the leaf is what identifies the code that wrote the line. The
 * full path stays in the title attribute.
 */
function shortTarget(target: string): string {
  const leaf = target.split("::").pop() ?? target;
  return leaf === "agento_lib" ? target : leaf;
}
