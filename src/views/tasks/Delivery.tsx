/* ============================================================================
   The Tasks form's Delivery section (#638): where a run's output is copied to.

   Every destination rides in the whole-task draft like any other field — the
   `PUT` is replace (#540) — so nothing here saves anything. Two rules hold it
   together:

   * **A stored destination is never dropped.** A row whose integration was
     deleted, disabled or de-authorised still renders with its channels and
     `when` intact, and carries a warning instead; saving without touching it
     round-trips it unchanged. Filtering it out would delete the user's
     configuration on the next save, silently.
   * **Warnings wait for `/integrations`.** Until that list has loaded, no row
     can be judged, so none is — a slow fetch must not flash "missing". An
     email row (#640) waits the same way for the SMTP settings.

   A Slack row with an integration chosen picks its channels from the
   workspace's own list (#641); `SlackChannelPicker` falls back to the
   comma-separated field when that list cannot be loaded.
   ========================================================================== */

import { useCallback, useEffect, useRef, useState } from "react";
import type {
  DeliveryWhen,
  Integration,
  SlackChannel,
  TaskDestination,
} from "../../lib/types";
import { api } from "../../lib/api";
import { SlackChannelPicker } from "../../components/SlackChannelPicker";
import { Dropdown, FormRow, FormSection, Segmented } from "../../components/ui";
import { Icon } from "../../lib/icons";
import { connectionState } from "../integrations/catalog";

type DestinationType = TaskDestination["type"];

/** What differs between the destination types, and nothing else — the row,
 *  the warning and the summary are one implementation over this table. */
const KINDS: Record<
  DestinationType,
  {
    label: string;
    /** Whether the type delivers through an integration; email sends through
     *  the SMTP server from Settings → Notifications instead. */
    integration: boolean;
    /** The ids field inside the type's sub-object. */
    ids: "channel_ids" | "chat_ids" | "recipients";
    idsLabel: string;
    noun: string;
    help: string;
    placeholder: string;
    /** Whether delivery also refuses an integration that is not connected.
     *  Telegram's sender checks `enabled` only, as its webhook does. */
    needsAuth: boolean;
  }
> = {
  slack: {
    label: "Slack",
    integration: true,
    ids: "channel_ids",
    idsLabel: "Channel IDs",
    noun: "channel",
    help: "Comma-separated channel IDs, e.g. C0123ABCD. Invite the bot to each channel.",
    placeholder: "C0123ABCD, C0456EFGH",
    needsAuth: true,
  },
  telegram: {
    label: "Telegram",
    integration: true,
    ids: "chat_ids",
    idsLabel: "Chat IDs",
    noun: "chat",
    help: "Comma-separated numeric chat IDs, e.g. -1001234567890 for a channel. The bot can only message a chat someone there has started it in, or that it was added to.",
    placeholder: "-1001234567890, 123456789",
    needsAuth: false,
  },
  email: {
    label: "Email",
    integration: false,
    ids: "recipients",
    idsLabel: "Recipients",
    noun: "recipient",
    help: "Comma-separated addresses. One message to all of them, so every recipient sees the others.",
    placeholder: "alice@example.com, bob@example.com",
    needsAuth: false,
  },
};

/** `enabled && authenticated` — the predicate delivery itself applies before
 *  it sends, so the form never offers an integration a run would skip. */
export function isUsableSlack(i: Integration): boolean {
  return i.type === "slack" && i.enabled && i.authenticated;
}

/** A connected Telegram integration (#639). */
export function isUsableTelegram(i: Integration): boolean {
  return i.type === "telegram" && i.enabled && i.authenticated;
}

/** Whether `i` can be offered as any destination — what decides whether the
 *  Delivery section shows at all for a task with none yet. */
export function isUsableDestination(i: Integration): boolean {
  return isUsableSlack(i) || isUsableTelegram(i);
}

/** The channel or chat field's text as the wire wants it: trimmed, empties
 *  dropped. */
export function parseChannelIds(text: string): string[] {
  return text
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

/** The entry's own sub-object, whichever type it is. */
function config(dest: TaskDestination): { integration_id: string; ids: string[] } {
  if (dest.type === "email") {
    return { integration_id: "", ids: dest.email?.recipients ?? [] };
  }
  const sub = dest.type === "telegram" ? dest.telegram : dest.slack;
  const ids = dest.type === "telegram" ? dest.telegram?.chat_ids : dest.slack?.channel_ids;
  return { integration_id: sub?.integration_id ?? "", ids: ids ?? [] };
}

/** The warning on an email destination with no SMTP server to send through;
 *  a run then records it as skipped. */
const SMTP_MISSING =
  "SMTP is not configured, so nothing is sent until it is set up in Settings → Notifications.";

/**
 * Why a stored destination will not deliver, or `null` when it will (or when
 * that cannot be told yet). `integrations` is `null` until the list loads, and
 * `smtpConfigured` until the notification settings do.
 */
export function destinationWarning(
  dest: TaskDestination,
  integrations: Integration[] | null,
  smtpConfigured: boolean | null = null
): string | null {
  if (dest.type === "email") return smtpConfigured === false ? SMTP_MISSING : null;
  const kind = KINDS[dest.type] ?? KINDS.slack;
  const id = config(dest).integration_id;
  if (!integrations || !id) return null;
  const integration = integrations.find((i) => i.id === id);
  if (!integration) {
    return "This integration no longer exists. Choose another one or remove this destination.";
  }
  if (integration.type !== dest.type) return `This integration is not a ${kind.label} integration.`;
  if (!integration.enabled) {
    return "This integration is disabled, so nothing is delivered until it is enabled.";
  }
  if (kind.needsAuth && !integration.authenticated) {
    return `${connectionState(false).label}: nothing is delivered until it is reconnected in Integrations.`;
  }
  return null;
}

function plural(n: number, noun: string): string {
  return `${n} ${noun}${n === 1 ? "" : "s"}`;
}

/** Delivery, collapsed: `none`, `Slack · 2 channels`,
 *  `Slack · 2 destinations · 5 channels`, or with both types
 *  `Slack, Telegram · 2 destinations · 1 channel · 3 chats`; ` · ⚠` when any
 *  row has a warning, so a broken destination is never hidden behind a closed
 *  header. */
export function deliverySummary(
  dests: TaskDestination[],
  integrations: Integration[] | null,
  smtpConfigured: boolean | null = null
): string {
  if (dests.length === 0) return "none";
  const types = (Object.keys(KINDS) as DestinationType[]).filter((t) =>
    dests.some((d) => d.type === t)
  );
  const parts = [types.map((t) => KINDS[t].label).join(", ")];
  if (dests.length > 1) parts.push(plural(dests.length, "destination"));
  for (const t of types) {
    const n = dests
      .filter((d) => d.type === t)
      .reduce((sum, d) => sum + config(d).ids.length, 0);
    parts.push(plural(n, KINDS[t].noun));
  }
  if (dests.some((d) => destinationWarning(d, integrations, smtpConfigured))) parts.push("⚠");
  return parts.join(" · ");
}

/** A new row: the first usable integration of `type`, no ids yet. */
function blankDestination(
  type: DestinationType,
  integrations: Integration[] | null
): TaskDestination {
  if (type === "email") return { type, when: "success", email: { recipients: [] } };
  const usable = type === "telegram" ? isUsableTelegram : isUsableSlack;
  const integration_id = integrations?.find(usable)?.id ?? "";
  return type === "telegram"
    ? { type, when: "success", telegram: { integration_id, chat_ids: [] } }
    : { type, when: "success", slack: { integration_id, channel_ids: [] } };
}

const WHEN_OPTIONS: { value: DeliveryWhen; label: string }[] = [
  { value: "success", label: "On success" },
  { value: "always", label: "Always" },
];

export function DeliverySection({
  destinations,
  integrations,
  integrationsError,
  smtpConfigured = null,
  open,
  onToggle,
  onChange,
}: {
  destinations: TaskDestination[];
  /** `null` while `/integrations` is loading or failed to load. */
  integrations: Integration[] | null;
  integrationsError?: string;
  /** Whether Settings → Notifications has an SMTP server; `null` until known. */
  smtpConfigured?: boolean | null;
  open: boolean;
  onToggle(): void;
  onChange(next: TaskDestination[]): void;
}) {
  // One `conversations.list` walk per integration for as long as the form is
  // open, shared by every row that picks it: Slack allows about 20 a minute.
  // A failure is forgotten, so Retry asks again.
  const channelCache = useRef(new Map<string, Promise<SlackChannel[] | null>>());
  const loadChannels = useCallback((id: string) => {
    const cache = channelCache.current;
    let pending = cache.get(id);
    if (!pending) {
      pending = api.get<SlackChannel[] | null>(
        `/integrations/${encodeURIComponent(id)}/slack/channels`
      );
      pending.catch(() => cache.delete(id));
      cache.set(id, pending);
    }
    return pending;
  }, []);
  const all = integrations ?? [];
  const hasTelegram = all.some(isUsableTelegram);
  // Slack stays offered whenever no other type is, which is the section as it
  // was before a second type existed — but not beside email alone, where it
  // would add a row with no integration to choose.
  const addable: DestinationType[] = [
    ...(all.some(isUsableSlack) || (!hasTelegram && !smtpConfigured) ? (["slack"] as const) : []),
    ...(hasTelegram ? (["telegram"] as const) : []),
    ...(smtpConfigured ? (["email"] as const) : []),
  ];
  return (
    <FormSection
      title="Delivery"
      summary={deliverySummary(destinations, integrations, smtpConfigured)}
      collapsible
      open={open}
      onToggle={onToggle}
    >
      <div className="formrow__help">
        Agento keeps each run's output as usual (see Save output); every
        destination gets a copy. Slack posts go to shared channels, so everyone
        in them sees the output, and the bot has to be invited to each channel.
        Telegram messages go to each chat, as plain text. Email goes to every
        recipient as one message.
      </div>
      {smtpConfigured === false && !destinations.some((d) => d.type === "email") && (
        <div className="formrow__help">
          To deliver by email, set up an SMTP server in Settings → Notifications.
        </div>
      )}
      {integrationsError && (
        <div className="formerror">Couldn't load integrations: {integrationsError}</div>
      )}
      {destinations.map((dest, i) => (
        <DeliveryDestinationRow
          // Destinations carry no id of their own; the row resyncs its local
          // text from the stored value whenever the two disagree.
          key={i}
          dest={dest}
          candidates={all.filter((c) => c.type === dest.type)}
          loaded={integrations !== null}
          warning={destinationWarning(dest, integrations, smtpConfigured)}
          loadChannels={loadChannels}
          onChange={(next) =>
            onChange(destinations.map((d, j) => (j === i ? next : d)))
          }
          onRemove={() => onChange(destinations.filter((_, j) => j !== i))}
        />
      ))}
      <div className="row">
        {addable.map((type) => (
          <button
            key={type}
            type="button"
            className="btn btn--ghost"
            onClick={() => onChange([...destinations, blankDestination(type, integrations)])}
          >
            <Icon name="plus" size={13} />
            Add {KINDS[type].label} destination
          </button>
        ))}
      </div>
    </FormSection>
  );
}

function DeliveryDestinationRow({
  dest,
  candidates,
  loaded,
  warning,
  loadChannels,
  onChange,
  onRemove,
}: {
  dest: TaskDestination;
  /** Every integration of the row's type, usable or not — a warning covers
   *  the rest. */
  candidates: Integration[];
  /** Whether `candidates` is the real list yet; until it is, nothing is
   *  "missing". */
  loaded: boolean;
  warning: string | null;
  loadChannels(integrationId: string): Promise<SlackChannel[] | null>;
  onChange(next: TaskDestination): void;
  onRemove(): void;
}) {
  const kind = KINDS[dest.type] ?? KINDS.slack;
  const { integration_id: id, ids } = config(dest);
  const stored = ids.join(", ");
  // The raw text, kept locally so `C01, ` does not snap back to `C01`
  // mid-keystroke. It only follows the draft when the two actually differ —
  // a revert, a reload, or a row shifting up after a removal.
  const [text, setText] = useState(stored);
  useEffect(() => {
    if (parseChannelIds(text).join(", ") !== stored) setText(stored);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stored]);

  const known = candidates.some((i) => i.id === id);
  const options = [
    ...(id ? [] : [{ value: "", label: "Choose an integration" }]),
    ...(id && !known ? [{ value: id, label: loaded ? `${id} (missing)` : id }] : []),
    ...candidates.map((i) => ({ value: i.id, label: i.name || i.id })),
  ];

  function patch(next: { integration_id?: string; ids?: string[] }) {
    const integration_id = next.integration_id ?? id;
    const list = next.ids ?? ids;
    onChange(
      dest.type === "email"
        ? { ...dest, email: { recipients: list } }
        : dest.type === "telegram"
          ? { ...dest, telegram: { integration_id, chat_ids: list } }
          : { ...dest, slack: { integration_id, channel_ids: list } }
    );
  }

  const idsField = (
    <label className="field">
      <input
        className="mono"
        value={text}
        onChange={(e) => {
          setText(e.target.value);
          patch({ ids: parseChannelIds(e.target.value) });
        }}
        placeholder={kind.placeholder}
        spellCheck={false}
      />
    </label>
  );

  return (
    <div className="delivery__dest">
      <FormRow label={kind.label}>
        <div className="delivery__head">
          {kind.integration ? (
            <Dropdown
              className="delivery__pick"
              value={id}
              options={options}
              onChange={(integration_id) => patch({ integration_id })}
            />
          ) : (
            <span className="delivery__via">Sent with the SMTP server from Settings → Notifications</span>
          )}
          <button type="button" className="btn btn--ghost" onClick={onRemove}>
            Remove
          </button>
        </div>
        {warning && <div className="delivery__warning">{warning}</div>}
      </FormRow>
      {dest.type === "slack" && id ? (
        <FormRow
          label="Channels"
          help="Each channel gets the summary and a thread with the output. Invite the bot to each one."
        >
          <SlackChannelPicker
            integrationId={id}
            value={ids}
            onChange={(next) => patch({ ids: next })}
            load={loadChannels}
            fallback={idsField}
          />
        </FormRow>
      ) : (
        <FormRow label={kind.idsLabel} help={kind.help}>
          {idsField}
        </FormRow>
      )}
      <FormRow label="When">
        <Segmented<DeliveryWhen>
          value={dest.when || "success"}
          options={WHEN_OPTIONS}
          onChange={(when) => {
            if (when !== dest.when) onChange({ ...dest, when });
          }}
        />
      </FormRow>
    </div>
  );
}
