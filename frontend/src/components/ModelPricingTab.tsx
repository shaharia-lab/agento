import { useState, useEffect, useCallback } from 'react'
import { ChevronDown, ChevronRight, Plus, Pencil, Trash2, AlertCircle, Lock } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Badge } from '@/components/ui/badge'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { pricingApi } from '@/lib/api'
import type { PricedModel, PricingCatalog, PricingRate, PricingRateInput } from '@/types'
import {
  describeRateWindow,
  describeTierBands,
  emptyRateInput,
  formatRateDate,
  groupByProvider,
  hasRateOn,
  isTiered,
  rateToInput,
  summarizeRatePrices,
  validateRateInput,
} from '@/lib/pricing'

// A form is either appending a rate or correcting one in place. They are
// deliberately different modes rather than one "edit": appending leaves history
// priced at what it was charged, correcting rewrites it.
type FormMode = 'add' | 'correct'

interface OpenForm {
  mode: FormMode
  input: PricingRateInput
  /** Existing rates for the model, used to describe the window being created. */
  existing: PricingRate[]
}

/** One numeric rate field. */
function RateField({
  label,
  value,
  onChange,
  hint,
}: Readonly<{
  label: string
  value: number
  onChange: (v: number) => void
  hint?: string
}>) {
  return (
    <div className="flex flex-col gap-1.5">
      <Label className="text-xs font-medium text-zinc-700 dark:text-zinc-300">{label}</Label>
      <Input
        type="number"
        min={0}
        step="0.01"
        value={value}
        onChange={e => onChange(Number(e.target.value))}
        className="font-mono text-sm"
      />
      {hint && <p className="text-xs text-zinc-400">{hint}</p>}
    </div>
  )
}

/** The add/correct form. Inline, matching every other Settings tab. */
function RateForm({
  form,
  saving,
  error,
  onChange,
  onSubmit,
  onCancel,
}: Readonly<{
  form: OpenForm
  saving: boolean
  error: string | null
  onChange: (input: PricingRateInput) => void
  onSubmit: () => void
  onCancel: () => void
}>) {
  const { mode, input, existing } = form
  const set = (patch: Partial<PricingRateInput>) => onChange({ ...input, ...patch })
  const collision = mode === 'add' && hasRateOn(existing, input.effective_from)
  // Bands come from the seeded catalog and are not editable here. Saying so
  // matters: the fields below are the lowest band only, so on a tiered model
  // an edit leaves long-context requests priced by the untouched bands.
  const correctingTiered =
    mode === 'correct' &&
    existing.some(r => r.effective_from === input.effective_from && isTiered(r))

  return (
    <fieldset className="flex flex-col gap-4 rounded-md border border-zinc-200 dark:border-zinc-700 p-4">
      <legend className="px-1 text-xs font-medium text-zinc-500 dark:text-zinc-400">
        {mode === 'add' ? 'Add a rate' : 'Correct this rate'}
      </legend>

      {mode === 'correct' && (
        <div className="rounded-md border border-amber-200 bg-amber-50 dark:border-amber-800 dark:bg-amber-900/20 px-3 py-2 text-xs text-amber-800 dark:text-amber-300">
          Correcting a rate rewrites costs already reported for the window it covers. If the
          provider changed its price, add a new rate instead so past usage keeps what it was
          charged.
        </div>
      )}

      {correctingTiered && (
        <div className="rounded-md border border-amber-200 bg-amber-50 dark:border-amber-800 dark:bg-amber-900/20 px-3 py-2 text-xs text-amber-800 dark:text-amber-300">
          This model prices by context length. The rates below are its lowest band only — the higher
          bands ship with the catalog and are not editable here, so requests above the first bound
          keep their existing prices.
        </div>
      )}

      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
        <div className="flex flex-col gap-1.5">
          <Label className="text-xs font-medium text-zinc-700 dark:text-zinc-300">
            Model pattern
          </Label>
          <Input
            value={input.model_pattern}
            onChange={e => set({ model_pattern: e.target.value })}
            disabled={mode === 'correct'}
            placeholder="claude-opus-5"
            className="font-mono text-sm"
          />
        </div>
        <div className="flex flex-col gap-1.5">
          <Label className="text-xs font-medium text-zinc-700 dark:text-zinc-300">
            Effective from
          </Label>
          <Input
            type="date"
            value={input.effective_from}
            onChange={e => set({ effective_from: e.target.value })}
            disabled={mode === 'correct'}
            className="text-sm"
          />
        </div>
      </div>

      {mode === 'add' && (
        <p className="text-xs text-zinc-500 dark:text-zinc-400">
          {describeRateWindow(existing, input.effective_from)}
        </p>
      )}
      {collision && (
        <p className="text-xs text-amber-600 dark:text-amber-400">
          A rate already starts on that date. Correct it from the history below, or pick another
          date.
        </p>
      )}

      <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
        <RateField
          label="Input $/MTok"
          value={input.input_per_mtok}
          onChange={v => set({ input_per_mtok: v })}
        />
        <RateField
          label="Output $/MTok"
          value={input.output_per_mtok}
          onChange={v => set({ output_per_mtok: v })}
        />
        <RateField
          label="Cache read $/MTok"
          value={input.cache_read_per_mtok}
          onChange={v => set({ cache_read_per_mtok: v })}
        />
        <RateField
          label="Cache write 5m $/MTok"
          value={input.cache_write_5m_per_mtok}
          onChange={v => set({ cache_write_5m_per_mtok: v })}
          hint="Anthropic bills 1.25× input; other providers publish their own."
        />
        <RateField
          label="Cache write 1h $/MTok"
          value={input.cache_write_1h_per_mtok}
          onChange={v => set({ cache_write_1h_per_mtok: v })}
          hint="Anthropic bills 2× input; providers without TTL tiers use the same as 5m."
        />
        <div className="flex flex-col gap-1.5">
          <Label className="text-xs font-medium text-zinc-700 dark:text-zinc-300">Source</Label>
          <Input
            value={input.source}
            onChange={e => set({ source: e.target.value })}
            placeholder="https://provider/pricing · retrieved 2026-08-09"
            className="text-sm"
          />
          <p className="text-xs text-zinc-400">
            The page and date this rate came from, so the next maintainer can verify it.
          </p>
        </div>
      </div>

      <label className="flex items-center gap-2 text-xs text-zinc-600 dark:text-zinc-400">
        <input
          type="checkbox"
          checked={!input.billable}
          onChange={e =>
            set(
              e.target.checked
                ? {
                    billable: false,
                    input_per_mtok: 0,
                    output_per_mtok: 0,
                    cache_read_per_mtok: 0,
                    cache_write_5m_per_mtok: 0,
                    cache_write_1h_per_mtok: 0,
                  }
                : { billable: true },
            )
          }
        />
        This model costs nothing (a placeholder or embedding model, not an unfilled rate)
      </label>

      {error && (
        <div className="rounded-md border border-red-200 bg-red-50 dark:border-red-800 dark:bg-red-900/20 px-3 py-2 text-sm text-red-700 dark:text-red-400">
          {error}
        </div>
      )}

      <div className="flex gap-2">
        <Button
          size="sm"
          onClick={onSubmit}
          disabled={saving}
          className="bg-zinc-900 hover:bg-zinc-800 text-white dark:bg-zinc-100 dark:hover:bg-zinc-200 dark:text-zinc-900"
        >
          {saving ? 'Saving…' : mode === 'add' ? 'Add rate' : 'Correct rate'}
        </Button>
        <Button size="sm" variant="outline" onClick={onCancel} disabled={saving}>
          Cancel
        </Button>
      </div>
    </fieldset>
  )
}

/** One row of a model's rate history. */
function HistoryRow({
  rate,
  isCurrent,
  onCorrect,
  onDelete,
}: Readonly<{
  rate: PricingRate
  isCurrent: boolean
  onCorrect: () => void
  onDelete: () => void
}>) {
  return (
    <div className="flex items-start justify-between gap-3 border-t border-zinc-100 dark:border-zinc-800 py-2 first:border-t-0">
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-xs font-medium text-zinc-700 dark:text-zinc-300">
            from {formatRateDate(rate.effective_from)}
          </span>
          {isCurrent && (
            <Badge variant="secondary" className="text-[10px] py-0 h-4">
              in effect
            </Badge>
          )}
          {rate.user_modified && (
            <Badge variant="outline" className="text-[10px] py-0 h-4">
              edited
            </Badge>
          )}
          {rate.is_builtin && !rate.user_modified && (
            <span className="flex items-center gap-1 text-[10px] text-zinc-400">
              <Lock className="h-2.5 w-2.5" /> built-in
            </span>
          )}
          {rate.estimated && (
            <Badge variant="outline" className="text-[10px] py-0 h-4">
              estimated
            </Badge>
          )}
          {!rate.billable && (
            <Badge variant="outline" className="text-[10px] py-0 h-4">
              non-billable
            </Badge>
          )}
        </div>
        <div className="mt-0.5 font-mono text-xs text-zinc-500 dark:text-zinc-400">
          in ${rate.input_per_mtok} · out ${rate.output_per_mtok} · read ${rate.cache_read_per_mtok}{' '}
          · write ${rate.cache_write_5m_per_mtok}/$
          {rate.cache_write_1h_per_mtok}
          {isTiered(rate) && ' (lowest band)'}
        </div>
        {isTiered(rate) && (
          <div className="mt-1 space-y-0.5 border-l-2 border-zinc-200 pl-2 dark:border-zinc-700">
            <div className="text-[10px] uppercase tracking-wide text-zinc-400">
              by input length · all of a request bills at its band
            </div>
            {describeTierBands(rate).map(band => (
              <div key={band} className="font-mono text-xs text-zinc-500 dark:text-zinc-400">
                {band}
              </div>
            ))}
          </div>
        )}
        {rate.source && (
          <div className="mt-0.5 text-[11px] text-zinc-400 break-words">{rate.source}</div>
        )}
      </div>
      <div className="flex shrink-0 gap-1">
        <Button
          variant="ghost"
          size="sm"
          onClick={onCorrect}
          className="text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200"
          title="Correct this rate — rewrites costs already reported for its window"
        >
          <Pencil className="h-3 w-3" />
        </Button>
        <Button
          variant="ghost"
          size="sm"
          onClick={onDelete}
          className="text-zinc-400 hover:text-red-500"
          title="Delete this rate"
        >
          <Trash2 className="h-3 w-3" />
        </Button>
      </div>
    </div>
  )
}

/** One model: a summary line, and its rate history when expanded. */
function ModelRow({
  model,
  expanded,
  onToggle,
  onAdd,
  onCorrect,
  onDelete,
}: Readonly<{
  model: PricedModel
  expanded: boolean
  onToggle: () => void
  onAdd: () => void
  onCorrect: (rate: PricingRate) => void
  onDelete: (rate: PricingRate) => void
}>) {
  const current = model.current
  return (
    <div className="rounded-md border border-zinc-200 dark:border-zinc-700">
      <div className="flex items-center gap-2 px-3 py-2">
        <button
          type="button"
          onClick={onToggle}
          aria-expanded={expanded}
          className="flex min-w-0 flex-1 items-center gap-2 text-left"
        >
          {expanded ? (
            <ChevronDown className="h-3.5 w-3.5 shrink-0 text-zinc-400" />
          ) : (
            <ChevronRight className="h-3.5 w-3.5 shrink-0 text-zinc-400" />
          )}
          <span className="truncate font-mono text-sm text-zinc-800 dark:text-zinc-200">
            {model.model_pattern}
          </span>
          <span className="shrink-0 text-xs text-zinc-400">
            {model.rates.length} rate{model.rates.length === 1 ? '' : 's'}
          </span>
          <span className="ml-auto shrink-0 font-mono text-xs text-zinc-500 dark:text-zinc-400">
            {current ? summarizeRatePrices(current) : 'not yet in effect'}
          </span>
        </button>
        <Button variant="outline" size="sm" onClick={onAdd} className="shrink-0 gap-1 text-xs">
          <Plus className="h-3 w-3" />
          Add rate
        </Button>
      </div>

      {expanded && (
        <div className="border-t border-zinc-100 dark:border-zinc-800 px-3 py-2">
          {model.rates.map(r => (
            <HistoryRow
              key={`${r.model_pattern}-${r.effective_from}`}
              rate={r}
              isCurrent={current?.effective_from === r.effective_from}
              onCorrect={() => onCorrect(r)}
              onDelete={() => onDelete(r)}
            />
          ))}
        </div>
      )}
    </div>
  )
}

/** Models seen in real sessions with no rate at all — the tab's to-do list. */
function UnpricedSection({
  models,
  onPrice,
}: Readonly<{ models: string[]; onPrice: (pattern: string) => void }>) {
  if (models.length === 0) return null
  return (
    <div className="flex flex-col gap-2 rounded-md border border-amber-200 bg-amber-50 dark:border-amber-800 dark:bg-amber-900/20 p-3">
      <div className="flex items-center gap-2 text-xs font-medium text-amber-800 dark:text-amber-300">
        <AlertCircle className="h-3.5 w-3.5 shrink-0" />
        {models.length} model{models.length === 1 ? '' : 's'} in your sessions have no rate
      </div>
      <p className="text-xs text-amber-700 dark:text-amber-400">
        Their tokens are excluded from every cost total until you price them.
      </p>
      <div className="flex flex-wrap gap-2">
        {models.map(m => (
          <Button
            key={m}
            variant="outline"
            size="sm"
            onClick={() => onPrice(m)}
            className="gap-1 font-mono text-xs"
          >
            <Plus className="h-3 w-3" />
            {m}
          </Button>
        ))}
      </div>
    </div>
  )
}

export default function ModelPricingTab() {
  const [catalog, setCatalog] = useState<PricingCatalog | null>(null)
  const [expanded, setExpanded] = useState<Set<string>>(new Set())
  const [form, setForm] = useState<OpenForm | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<PricingRate | null>(null)
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [formError, setFormError] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [toast, setToast] = useState<string | null>(null)

  const showToast = (msg: string) => {
    setToast(msg)
    setTimeout(() => setToast(null), 4000)
  }

  const load = useCallback(async () => {
    try {
      setCatalog(await pricingApi.catalog())
      setError(null)
    } catch {
      setError('Failed to load the pricing catalog')
    } finally {
      setLoading(false)
    }
  }, [])

  useEffect(() => {
    load()
  }, [load])

  const toggle = (pattern: string) =>
    setExpanded(prev => {
      const next = new Set(prev)
      if (next.has(pattern)) next.delete(pattern)
      else next.add(pattern)
      return next
    })

  const openAdd = (model?: PricedModel, pattern?: string) => {
    setFormError(null)
    setForm({
      mode: 'add',
      input: emptyRateInput(model?.model_pattern ?? pattern ?? '', model?.provider ?? ''),
      existing: model?.rates ?? [],
    })
  }

  const openCorrect = (model: PricedModel, rate: PricingRate) => {
    setFormError(null)
    setForm({ mode: 'correct', input: rateToInput(rate), existing: model.rates })
  }

  const submit = async () => {
    if (!form) return
    const invalid = validateRateInput(form.input)
    if (invalid) {
      setFormError(invalid)
      return
    }
    setSaving(true)
    setFormError(null)
    try {
      if (form.mode === 'add') await pricingApi.addRate(form.input)
      else await pricingApi.correctRate(form.input)
      setForm(null)
      await load()
      showToast(
        'Rate saved — session costs recalculate in the background; the sessions list stays usable and updates itself',
      )
    } catch (err) {
      setFormError(err instanceof Error ? err.message : 'Failed to save the rate')
    } finally {
      setSaving(false)
    }
  }

  const confirmDelete = async () => {
    if (!deleteTarget) return
    const target = deleteTarget
    setDeleteTarget(null)
    try {
      await pricingApi.deleteRate(target.model_pattern, target.effective_from)
      await load()
      showToast(
        'Rate deleted — session costs recalculate in the background; the sessions list stays usable and updates itself',
      )
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete the rate')
    }
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <div className="text-sm text-zinc-400">Loading…</div>
      </div>
    )
  }

  return (
    <div className="flex flex-col gap-6">
      <div className="flex items-start justify-between gap-4">
        <div>
          <h2 className="text-sm font-semibold text-zinc-900 dark:text-zinc-100">Model Pricing</h2>
          <p className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
            Rates are effective-dated. Adding one leaves past usage priced at what it was charged;
            correcting one rewrites it.
          </p>
        </div>
        <Button variant="outline" size="sm" onClick={() => openAdd()} className="gap-1 text-xs">
          <Plus className="h-3 w-3" />
          Add model
        </Button>
      </div>

      {error && (
        <div className="rounded-md border border-red-200 bg-red-50 dark:border-red-800 dark:bg-red-900/20 px-3 py-2 text-sm text-red-700 dark:text-red-400">
          {error}
        </div>
      )}

      <UnpricedSection
        models={catalog?.unpriced_models ?? []}
        onPrice={pattern => openAdd(undefined, pattern)}
      />

      {form && (
        <RateForm
          form={form}
          saving={saving}
          error={formError}
          onChange={input => setForm(prev => (prev ? { ...prev, input } : prev))}
          onSubmit={submit}
          onCancel={() => setForm(null)}
        />
      )}

      {groupByProvider(catalog?.models ?? []).map(([provider, models]) => (
        <div key={provider} className="flex flex-col gap-2">
          <h3 className="text-xs font-medium uppercase tracking-wide text-zinc-400">{provider}</h3>
          {models.map(m => (
            <ModelRow
              key={m.model_pattern}
              model={m}
              expanded={expanded.has(m.model_pattern)}
              onToggle={() => toggle(m.model_pattern)}
              onAdd={() => openAdd(m)}
              onCorrect={r => openCorrect(m, r)}
              onDelete={setDeleteTarget}
            />
          ))}
        </div>
      ))}

      <AlertDialog
        open={deleteTarget !== null}
        onOpenChange={open => {
          if (!open) setDeleteTarget(null)
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete this rate?</AlertDialogTitle>
            <AlertDialogDescription>
              Sessions costed under it will fall back to whichever earlier rate applies, or become
              unpriced if there is none. This cannot be undone.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              className="bg-red-600 hover:bg-red-700 text-white"
              onClick={confirmDelete}
            >
              Delete
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>

      {toast && (
        <div className="fixed bottom-4 right-4 z-50 rounded-md bg-zinc-900 dark:bg-zinc-100 text-white dark:text-zinc-900 px-4 py-2 text-sm shadow-lg">
          {toast}
        </div>
      )}
    </div>
  )
}
