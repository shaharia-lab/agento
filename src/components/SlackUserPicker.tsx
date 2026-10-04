/* ============================================================================
   A Slack rule's allowed-users picker (#689): a searchable multi-select over
   `GET /api/integrations/{id}/slack/users`, writing the same list of user ids
   the comma-separated field did (#688). `SlackChannelPicker` (#641) is the
   pattern, and the two share one stylesheet.

   * **The list is fetched once per integration.** `users.list` is Slack's
     Tier 2 (about 20 calls a minute), so the fetch is keyed on the integration
     id alone and goes through the caller's `load`, which caches it.
   * **A stored id is never dropped.** One the list does not contain — a
     deactivated account, a bot, or a member past the route's page cap — stays
     selected as `U123 (not in list)` until the user removes it, and saves
     back unchanged.
   * **A pasted id is accepted.** A search that is itself a user id (`idPattern`,
     the write's own rule) and names nobody in the list offers an **Add** row,
     so a member the list does not show can still be allowed.
   * **A failed list degrades to `fallback`**, the comma-separated field, with
     the error beneath it. A Slack app without `users:read` is the common first
     case, and its rule still has to be saveable.
   ========================================================================== */

import { useMemo, useState, type ReactNode } from "react";
import type { SlackUser } from "../lib/types";
import { useResource } from "../lib/hooks";
import { Checkbox, Search } from "./ui";
import { Icon } from "../lib/icons";
import "../styles/channelpicker.css";

/** Rows rendered at once; search narrows the rest. A workspace can have ten
 *  thousand members, and the list is not virtualised. */
const MAX_ROWS = 200;

/** What a member is called: their real name, or their handle without one. */
function labelOf(u: SlackUser): string {
  return u.real_name || `@${u.name}`;
}

export function SlackUserPicker({
  integrationId,
  value,
  onChange,
  load,
  idPattern,
  fallback,
}: {
  integrationId: string;
  value: string[];
  onChange(ids: string[]): void;
  /** The integration's members; the caller caches it per integration. */
  load(integrationId: string): Promise<SlackUser[] | null>;
  /** One user id as the write accepts it. */
  idPattern: RegExp;
  /** What to show when the list cannot be loaded: the plain ids field. */
  fallback: ReactNode;
}) {
  const res = useResource(() => load(integrationId), [integrationId]);
  const [query, setQuery] = useState("");

  // Nothing while a fetch is in flight, so a switch of integration never
  // judges the new ids against the old list; a `null` answer is an empty one.
  const users = res.loading || res.error ? undefined : (res.data ?? []);
  const byId = useMemo(() => new Map((users ?? []).map((u) => [u.id, u])), [users]);
  const typed = query.trim();
  const filtered = useMemo(() => {
    const q = typed.replace(/^@/, "").toLowerCase();
    const all = users ?? [];
    return q
      ? all.filter(
          (u) =>
            u.name.toLowerCase().includes(q) ||
            u.real_name.toLowerCase().includes(q) ||
            u.id.toLowerCase() === q
        )
      : all;
  }, [users, typed]);

  if (res.error && !res.loading) {
    return (
      <>
        {fallback}
        <div className="chanpick__error">
          <span>Couldn't list members: {res.error}</span>
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
  // The typed text as an id of its own: offered only once the list is in, and
  // only when it names nobody the list or the selection already has.
  const pasted =
    users && idPattern.test(typed) && !byId.has(typed) && !selected.has(typed) ? typed : undefined;

  return (
    <div className="chanpick">
      {value.length > 0 && (
        <div className="chanpick__chips">
          {value.map((id) => {
            const u = byId.get(id);
            const label = u ? labelOf(u) : users ? `${id} (not in list)` : id;
            return (
              <span
                key={id}
                className={`chanpick__chip ${!u && users ? "chanpick__chip--gone" : ""}`}
                title={u ? `@${u.name} · ${id}` : undefined}
              >
                {label}
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
      <Search
        value={query}
        onChange={setQuery}
        placeholder="Search members to allow, or paste a user ID"
      />
      <div className="chanpick__list" aria-label="Allowed users">
        {!users ? (
          <div className="chanpick__note">Loading members…</div>
        ) : filtered.length === 0 && !pasted ? (
          <div className="chanpick__note">
            {users.length === 0 ? "The workspace lists no members." : "No member matches."}
          </div>
        ) : (
          filtered.slice(0, MAX_ROWS).map((u) => (
            <div key={u.id} className="chanpick__row" title={u.id}>
              <Checkbox on={selected.has(u.id)} onChange={(on) => toggle(u.id, on)} />
              <span className="chanpick__label" onClick={() => toggle(u.id, !selected.has(u.id))}>
                {labelOf(u)}
                {u.real_name && <span className="chanpick__handle">@{u.name}</span>}
              </span>
            </div>
          ))
        )}
        {pasted && (
          <div className="chanpick__row">
            <span className="chanpick__label mono">{pasted}</span>
            <button
              type="button"
              className="btn btn--ghost"
              onClick={() => {
                toggle(pasted, true);
                setQuery("");
              }}
            >
              <Icon name="plus" size={13} />
              Add
            </button>
          </div>
        )}
        {filtered.length > MAX_ROWS && (
          <div className="chanpick__note">
            {filtered.length - MAX_ROWS} more — search to narrow the list.
          </div>
        )}
      </div>
    </div>
  );
}
