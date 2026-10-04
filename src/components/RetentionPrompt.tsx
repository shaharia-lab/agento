import { useState } from "react";
import { api } from "../lib/api";
import { daysText, extendOptions, retentionFile } from "../lib/claudeRetention";
import { tildePath } from "../lib/format";
import { describeError, useResource } from "../lib/hooks";
import { Icon } from "../lib/icons";
import type {
  ClaudeRetention,
  ClaudeRetentionDir,
  ClaudeRetentionRequest,
  SettingsResponse,
} from "../lib/types";
import { Dropdown } from "./ui";
import "../styles/retentionprompt.css";

type Choice = "keep" | "extend" | "nothing";

/** The Dropdown value for a dir the user chose not to extend. */
const LEAVE = "";

const CHOICES: { id: Choice; label: string; note: string }[] = [
  {
    id: "keep",
    label: "Keep summaries in Agento",
    note: "Agento keeps each session's summary after its transcript is gone. Changes nothing in Claude Code.",
  },
  {
    id: "extend",
    label: "Extend Claude Code's retention",
    note: "Raises cleanupPeriodDays in settings.json, so the transcripts themselves are kept longer.",
  },
  { id: "nothing", label: "Do nothing", note: "" },
];

/** A dir whose retention is known, which is what the prompt has to report. */
function known(d: ClaudeRetentionDir): boolean {
  return d.source !== "unknown" && d.cleanup_period_days !== undefined;
}

/** `Claude Code deletes transcripts after N days.`, across every known dir. */
function headline(dirs: ClaudeRetentionDir[]): string {
  const days = dirs
    .filter(known)
    .map((d) => d.cleanup_period_days as number)
    // Claude Code rejects 0 and skips cleanup, so 0 is not "after 0 days".
    .filter((n) => n > 0);
  if (days.length === 0) {
    return "Claude Code is not deleting transcripts: cleanupPeriodDays is 0, which it rejects.";
  }
  const low = Math.min(...days);
  const high = Math.max(...days);
  return low === high
    ? `Claude Code deletes transcripts after ${daysText(low)}.`
    : `Claude Code deletes transcripts after ${low} to ${daysText(high)}, depending on the config directory.`;
}

/** Why a dir offers no extend, in visible text beside it (#713). */
function noExtendReason(d: ClaudeRetentionDir): string {
  if (!known(d)) return `could not be read (${d.reason ?? "no reason was given"})`;
  if (d.cleanup_period_days === 0) {
    return "is 0, which Claude Code rejects, so it never cleans up; any number would start deleting transcripts";
  }
  return `is already ${daysText(d.cleanup_period_days as number)}, the longest Agento offers or more`;
}

/**
 * The one-time retention prompt (#720): tells the user Claude Code deletes
 * transcripts after N days and offers keep / extend / do nothing. Shown until
 * it is answered or dismissed, which `claude_retention_prompt_answered`
 * records. **It has no control that can lower the value**: extend offers only
 * presets strictly above the current one (`extendOptions`), and the route
 * refuses anything else for any caller.
 */
export function RetentionPrompt({ onExtended }: { onExtended(): void }) {
  const settings = useResource(
    (signal) => api.get<SettingsResponse>("/settings", signal),
    []
  );
  // Nothing below is mounted, and no `settings.json` is read, once answered.
  if (!settings.data || settings.data.settings.claude_retention_prompt_answered) return null;
  return <UnansweredPrompt onExtended={onExtended} />;
}

function UnansweredPrompt({ onExtended }: { onExtended(): void }) {
  const retention = useResource(
    (signal) => api.get<ClaudeRetention>("/settings/claude-retention", signal),
    []
  );

  const [choice, setChoice] = useState<Choice>("keep");
  // What each dir is extended to, by config dir. A dir not in here takes its
  // lowest offered preset; LEAVE means the user opted that dir out.
  const [picked, setPicked] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [hidden, setHidden] = useState(false);
  // Every dir this prompt has already raised, as the `PUT` answered it. A
  // retry after a later step failed must not raise one of these again: the
  // dir now offers only higher presets, and a write cannot be taken back.
  const [written, setWritten] = useState<Record<string, ClaudeRetentionDir>>({});
  // Set once the extend and the answered flag are both stored: the closing state.
  const [extended, setExtended] = useState(false);

  const dirs = retention.data?.dirs ?? [];
  if (hidden) return null;
  if (!extended && !dirs.some(known)) return null;

  const extendable = dirs.filter(
    (d) => !written[d.config_dir] && extendOptions(d).length > 0
  );
  const target = (d: ClaudeRetentionDir): string => {
    const options = extendOptions(d).map((o) => String(o.days));
    const choice = picked[d.config_dir];
    // A pick the dir no longer offers (its value rose since) is not a pick.
    if (choice === LEAVE || (choice !== undefined && options.includes(choice))) return choice;
    return options[0] ?? LEAVE;
  };

  /**
   * Record the prompt as answered. Its own route (#751), which stores that one
   * flag: a `PUT /settings` replaces the whole row, and posting back what the
   * `GET` answered would store the defaults it had filled in.
   */
  async function markAnswered() {
    await api.post("/settings/retention-prompt/answered");
  }

  async function dismiss() {
    // Hidden whatever happens: a failed save must not pin the prompt open.
    setHidden(true);
    if (extended) return;
    try {
      await markAnswered();
    } catch {
      // Not answered, so the prompt comes back on the next launch.
    }
  }

  async function confirm() {
    setBusy(true);
    setError(undefined);
    try {
      if (choice !== "extend") {
        await markAnswered();
        setHidden(true);
        return;
      }
      const chosen = extendable.filter((d) => target(d) !== LEAVE);
      if (chosen.length === 0 && Object.keys(written).length === 0) {
        setError("Choose a retention for at least one config directory.");
        return;
      }
      for (const d of chosen) {
        const body: ClaudeRetentionRequest = {
          config_dir: d.config_dir,
          cleanup_period_days: Number(target(d)),
        };
        const answer = await api.put<ClaudeRetention>("/settings/claude-retention", body);
        const now = answer.dirs?.find((x) => x.config_dir === d.config_dir);
        if (now) setWritten((w) => ({ ...w, [d.config_dir]: now }));
        onExtended();
      }
      await markAnswered();
      setExtended(true);
    } catch (err) {
      setError(describeError(err));
      // A dir written before the failure shows its new value.
      retention.reload();
    } finally {
      setBusy(false);
    }
  }

  if (extended) {
    return (
      <div className="banner retention-prompt">
        <div className="retention-prompt__head">
          <Icon name="check" size={14} />
          <span>
            {Object.values(written)
              .map(
                (d) =>
                  `Claude Code's retention is now ${daysText(d.cleanup_period_days ?? 0)} in ${retentionFile(d)}.`
              )
              .join(" ")}{" "}
            Claude Code applies the new value itself, and managed or project
            settings can override it.
          </span>
          <button className="iconbtn" onClick={dismiss} title="Dismiss">
            <Icon name="close" size={13} />
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="banner retention-prompt">
      <div className="retention-prompt__head">
        <Icon name="alert" size={14} />
        <span>{headline(dirs)}</span>
        <button className="iconbtn" onClick={dismiss} title="Dismiss" disabled={busy}>
          <Icon name="close" size={13} />
        </button>
      </div>

      <div className="retention-prompt__options" role="radiogroup" aria-label="Transcript retention">
        {CHOICES.map((c) => {
          const unavailable =
            c.id === "extend" && extendable.length === 0 && Object.keys(written).length === 0;
          return (
            <div key={c.id} className="col" style={{ gap: "var(--sp-2)" }}>
              <label className="retention-prompt__option">
                <input
                  type="radio"
                  name="retention-prompt"
                  checked={choice === c.id}
                  disabled={busy || unavailable}
                  onChange={() => {
                    setChoice(c.id);
                    setError(undefined);
                  }}
                />
                <span>{c.label}</span>
                {c.note && <span className="retention-prompt__note">{c.note}</span>}
              </label>
              {c.id === "extend" && (unavailable || choice === "extend") && (
                <div className="retention-prompt__dirs">
                  {dirs.map((d) => {
                    const options = extendOptions(d);
                    const done = written[d.config_dir];
                    return (
                      <div key={d.config_dir} className="retention-prompt__dir" title={d.config_dir}>
                        {done ? (
                          <span className="retention-prompt__note">
                            {tildePath(d.config_dir)}: extended to{" "}
                            {daysText(done.cleanup_period_days ?? 0)}.
                          </span>
                        ) : options.length === 0 ? (
                          <span className="retention-prompt__note">
                            {tildePath(d.config_dir)}: nothing to extend. cleanupPeriodDays{" "}
                            {noExtendReason(d)}.
                          </span>
                        ) : (
                          <>
                            <span>
                              {tildePath(d.config_dir)}: now{" "}
                              {daysText(d.cleanup_period_days as number)}
                              {d.source === "default" ? " (the default)" : ""}, extend to
                            </span>
                            <Dropdown
                              small
                              disabled={busy}
                              ariaLabel={`Retention for ${d.config_dir}`}
                              value={target(d)}
                              onChange={(v) =>
                                setPicked((p) => ({ ...p, [d.config_dir]: v }))
                              }
                              options={[
                                ...options.map((o) => ({
                                  value: String(o.days),
                                  label: o.label,
                                })),
                                ...(dirs.length > 1
                                  ? [{ value: LEAVE, label: "Leave as it is" }]
                                  : []),
                              ]}
                            />
                          </>
                        )}
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
          );
        })}
      </div>

      {error && (
        <div className="msgline msgline--error" style={{ marginLeft: "calc(14px + var(--sp-4))" }}>
          {error}
        </div>
      )}

      <div className="retention-prompt__actions">
        <button className="btn btn--primary btn--sm" onClick={confirm} disabled={busy}>
          {busy ? "Saving…" : "Confirm"}
        </button>
      </div>
    </div>
  );
}
