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
     can be judged, so none is — a slow fetch must not flash "missing".
   ========================================================================== */

import { useEffect, useState } from "react";
import type { DeliveryWhen, Integration, TaskDestination } from "../../lib/types";
import { Dropdown, FormRow, FormSection, Segmented } from "../../components/ui";
import { Icon } from "../../lib/icons";
import { connectionState } from "../integrations/catalog";

/** `enabled && authenticated` — the predicate delivery itself applies before
 *  it sends, so the form never offers an integration a run would skip. */
export function isUsableSlack(i: Integration): boolean {
  return i.type === "slack" && i.enabled && i.authenticated;
}

/** The channel field's text as the wire wants it: trimmed, empties dropped. */
export function parseChannelIds(text: string): string[] {
  return text
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

/**
 * Why a stored destination will not deliver, or `null` when it will (or when
 * that cannot be told yet). `integrations` is `null` until the list loads.
 */
export function destinationWarning(
  dest: TaskDestination,
  integrations: Integration[] | null
): string | null {
  const id = dest.slack?.integration_id ?? "";
  if (!integrations || !id) return null;
  const integration = integrations.find((i) => i.id === id);
  if (!integration) {
    return "This integration no longer exists. Choose another one or remove this destination.";
  }
  if (integration.type !== "slack") return "This integration is not a Slack integration.";
  if (!integration.enabled) {
    return "This integration is disabled, so nothing is delivered until it is enabled.";
  }
  if (!integration.authenticated) {
    return `${connectionState(false).label}: nothing is delivered until it is reconnected in Integrations.`;
  }
  return null;
}

function plural(n: number, noun: string): string {
  return `${n} ${noun}${n === 1 ? "" : "s"}`;
}

/** Delivery, collapsed: `none`, `Slack · 2 channels`, or
 *  `Slack · 2 destinations · 5 channels`; ` · ⚠` when any row has a warning,
 *  so a broken destination is never hidden behind a closed header. */
export function deliverySummary(
  dests: TaskDestination[],
  integrations: Integration[] | null
): string {
  if (dests.length === 0) return "none";
  const channels = dests.reduce((n, d) => n + (d.slack?.channel_ids?.length ?? 0), 0);
  const parts = ["Slack"];
  if (dests.length > 1) parts.push(plural(dests.length, "destination"));
  parts.push(plural(channels, "channel"));
  if (dests.some((d) => destinationWarning(d, integrations))) parts.push("⚠");
  return parts.join(" · ");
}

/** A new row: the first usable Slack integration, no channels yet. */
function blankDestination(integrations: Integration[] | null): TaskDestination {
  return {
    type: "slack",
    when: "success",
    slack: {
      integration_id: integrations?.find(isUsableSlack)?.id ?? "",
      channel_ids: [],
    },
  };
}

const WHEN_OPTIONS: { value: DeliveryWhen; label: string }[] = [
  { value: "success", label: "On success" },
  { value: "always", label: "Always" },
];

export function DeliverySection({
  destinations,
  integrations,
  integrationsError,
  open,
  onToggle,
  onChange,
}: {
  destinations: TaskDestination[];
  /** `null` while `/integrations` is loading or failed to load. */
  integrations: Integration[] | null;
  integrationsError?: string;
  open: boolean;
  onToggle(): void;
  onChange(next: TaskDestination[]): void;
}) {
  const slack = (integrations ?? []).filter((i) => i.type === "slack");
  return (
    <FormSection
      title="Delivery"
      summary={deliverySummary(destinations, integrations)}
      collapsible
      open={open}
      onToggle={onToggle}
    >
      <div className="formrow__help">
        Agento keeps each run's output as usual (see Save output); every
        destination gets a copy. Slack posts go to shared channels, so everyone
        in them sees the output, and the bot has to be invited to each channel.
      </div>
      {integrationsError && (
        <div className="formerror">Couldn't load integrations: {integrationsError}</div>
      )}
      {destinations.map((dest, i) => (
        <DeliveryDestinationRow
          // Destinations carry no id of their own; the row resyncs its local
          // text from the stored value whenever the two disagree.
          key={i}
          dest={dest}
          slack={slack}
          loaded={integrations !== null}
          warning={destinationWarning(dest, integrations)}
          onChange={(next) =>
            onChange(destinations.map((d, j) => (j === i ? next : d)))
          }
          onRemove={() => onChange(destinations.filter((_, j) => j !== i))}
        />
      ))}
      <div>
        <button
          type="button"
          className="btn btn--ghost"
          onClick={() => onChange([...destinations, blankDestination(integrations)])}
        >
          <Icon name="plus" size={13} />
          Add Slack destination
        </button>
      </div>
    </FormSection>
  );
}

function DeliveryDestinationRow({
  dest,
  slack,
  loaded,
  warning,
  onChange,
  onRemove,
}: {
  dest: TaskDestination;
  /** Every Slack integration, usable or not — a warning covers the rest. */
  slack: Integration[];
  /** Whether `slack` is the real list yet; until it is, nothing is "missing". */
  loaded: boolean;
  warning: string | null;
  onChange(next: TaskDestination): void;
  onRemove(): void;
}) {
  const id = dest.slack?.integration_id ?? "";
  const ids = dest.slack?.channel_ids ?? [];
  const stored = ids.join(", ");
  // The raw text, kept locally so `C01, ` does not snap back to `C01`
  // mid-keystroke. It only follows the draft when the two actually differ —
  // a revert, a reload, or a row shifting up after a removal.
  const [text, setText] = useState(stored);
  useEffect(() => {
    if (parseChannelIds(text).join(", ") !== stored) setText(stored);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [stored]);

  const known = slack.some((i) => i.id === id);
  const options = [
    ...(id ? [] : [{ value: "", label: "Choose an integration" }]),
    ...(id && !known ? [{ value: id, label: loaded ? `${id} (missing)` : id }] : []),
    ...slack.map((i) => ({ value: i.id, label: i.name || i.id })),
  ];

  function setSlack(patch: Partial<NonNullable<TaskDestination["slack"]>>) {
    onChange({
      ...dest,
      slack: { integration_id: id, channel_ids: ids, ...patch },
    });
  }

  return (
    <div className="delivery__dest">
      <FormRow label="Slack">
        <div className="inline">
          <Dropdown
            value={id}
            options={options}
            onChange={(integration_id) => setSlack({ integration_id })}
          />
          <div className="spacer" />
          <button type="button" className="btn btn--ghost" onClick={onRemove}>
            Remove
          </button>
        </div>
        {warning && <div className="delivery__warning">{warning}</div>}
      </FormRow>
      <FormRow
        label="Channel IDs"
        help="Comma-separated channel IDs, e.g. C0123ABCD. Invite the bot to each channel."
      >
        <label className="field">
          <input
            className="mono"
            value={text}
            onChange={(e) => {
              setText(e.target.value);
              setSlack({ channel_ids: parseChannelIds(e.target.value) });
            }}
            placeholder="C0123ABCD, C0456EFGH"
            spellCheck={false}
          />
        </label>
      </FormRow>
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
