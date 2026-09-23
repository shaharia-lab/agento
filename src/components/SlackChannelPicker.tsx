/* ============================================================================
   The Slack destination's channel picker (#641): a searchable multi-select
   over `GET /api/integrations/{id}/slack/channels`, writing the same
   `channel_ids` list the comma-separated field did.

   * **The list is fetched once per integration.** `conversations.list` is
     Slack's Tier 2 (about 20 calls a minute), so the fetch is keyed on the
     integration id alone and goes through the caller's `load`, which shares
     one request between every row that picks the same integration.
   * **A stored id is never dropped.** One the list does not contain —
     archived, private and invisible to the bot, or deleted — stays selected as
     `C123 (not found)` until the user removes it, and saves back unchanged.
   * **A failed list degrades to `fallback`**, the comma-separated field, with
     the error beneath it, so a workspace the route cannot read still saves.
   * **`is_member: false` is marked**, because posting there fails with
     `not_in_channel` (#637); the picker says so at setup instead.
   ========================================================================== */

import { useMemo, useState, type ReactNode } from "react";
import type { SlackChannel } from "../lib/types";
import { useResource } from "../lib/hooks";
import { Checkbox, Search } from "./ui";
import { Icon } from "../lib/icons";
import "../styles/channelpicker.css";

/** Rows rendered at once; search narrows the rest. A workspace can have ten
 *  thousand channels, and the list is not virtualised. */
const MAX_ROWS = 200;

const INVITE_HINT = "Invite @app with /invite, or deliveries to it fail with not_in_channel.";

export function SlackChannelPicker({
  integrationId,
  value,
  onChange,
  load,
  fallback,
}: {
  integrationId: string;
  value: string[];
  onChange(ids: string[]): void;
  /** The integration's channels; the caller caches it per integration. */
  load(integrationId: string): Promise<SlackChannel[] | null>;
  /** What to show when the list cannot be loaded: the plain ids field. */
  fallback: ReactNode;
}) {
  const res = useResource(() => load(integrationId), [integrationId]);
  const [query, setQuery] = useState("");

  // Nothing while a fetch is in flight, so a switch of integration never
  // judges the new ids against the old list; a `null` answer is an empty one.
  const channels = res.loading || res.error ? undefined : (res.data ?? []);
  const byId = useMemo(
    () => new Map((channels ?? []).map((c) => [c.id, c])),
    [channels]
  );
  const filtered = useMemo(() => {
    const q = query.trim().replace(/^#/, "").toLowerCase();
    const all = channels ?? [];
    return q
      ? all.filter((c) => c.name.toLowerCase().includes(q) || c.id.toLowerCase() === q)
      : all;
  }, [channels, query]);

  if (res.error && !res.loading) {
    return (
      <>
        {fallback}
        <div className="chanpick__error">
          <span>Couldn't list channels: {res.error}</span>
          <button type="button" className="btn btn--ghost" onClick={res.reload}>
            <Icon name="refresh" size={13} />
            Retry
          </button>
        </div>
      </>
    );
  }

  const selected = new Set(value);
  const toggle = (id: string, on: boolean) =>
    onChange(on ? [...value, id] : value.filter((v) => v !== id));
  const outside = value
    .map((id) => byId.get(id))
    .filter((c): c is SlackChannel => !!c && !c.is_member);

  return (
    <div className="chanpick">
      {value.length > 0 && (
        <div className="chanpick__chips">
          {value.map((id) => {
            const c = byId.get(id);
            const label = c ? `#${c.name}` : channels ? `${id} (not found)` : id;
            return (
              <span
                key={id}
                className={`chanpick__chip ${!c && channels ? "chanpick__chip--gone" : ""}`}
                title={c ? id : undefined}
              >
                {label}
                {c && !c.is_member && <Icon name="alert" size={11} />}
                <button
                  type="button"
                  className="iconbtn chanpick__remove"
                  aria-label={`Remove ${label}`}
                  onClick={() => toggle(id, false)}
                >
                  <Icon name="close" size={10} />
                </button>
              </span>
            );
          })}
        </div>
      )}
      <Search value={query} onChange={setQuery} placeholder="Search channels" />
      <div className="chanpick__list">
        {!channels ? (
          <div className="chanpick__note">Loading channels…</div>
        ) : filtered.length === 0 ? (
          <div className="chanpick__note">
            {channels.length === 0 ? "The bot can see no channels." : "No channel matches."}
          </div>
        ) : (
          filtered.slice(0, MAX_ROWS).map((c) => (
            <div key={c.id} className="chanpick__row" title={c.is_member ? c.id : INVITE_HINT}>
              <Checkbox on={selected.has(c.id)} onChange={(on) => toggle(c.id, on)} />
              <span
                className="chanpick__label"
                onClick={() => toggle(c.id, !selected.has(c.id))}
              >
                #{c.name}
              </span>
              {c.is_private && <span className="badge">private</span>}
              {!c.is_member && <span className="badge badge--amber">bot not in channel</span>}
            </div>
          ))
        )}
        {filtered.length > MAX_ROWS && (
          <div className="chanpick__note">
            {filtered.length - MAX_ROWS} more — search to narrow the list.
          </div>
        )}
      </div>
      {outside.length > 0 && (
        <div className="chanpick__hint">
          The bot is not in {outside.map((c) => `#${c.name}`).join(", ")}. {INVITE_HINT}
        </div>
      )}
    </div>
  );
}
