import { forwardRef } from 'react'

interface SwitchProps {
  checked: boolean
  onCheckedChange: (checked: boolean) => void
  disabled?: boolean
  id?: string
  className?: string
}

const Switch = forwardRef<HTMLButtonElement, SwitchProps>(
  ({ checked, onCheckedChange, disabled, id, className = '' }, ref) => {
    return (
      <button
        ref={ref}
        id={id}
        role="switch"
        aria-checked={checked}
        disabled={disabled}
        onClick={() => onCheckedChange(!checked)}
        className={`
          relative inline-flex h-5 w-9 shrink-0 cursor-pointer items-center rounded-full
          border-2 border-transparent transition-colors focus-visible:outline-none
          focus-visible:ring-2 focus-visible:ring-zinc-900 dark:focus-visible:ring-zinc-400
          focus-visible:ring-offset-2 focus-visible:ring-offset-white dark:focus-visible:ring-offset-zinc-950
          disabled:cursor-not-allowed disabled:opacity-50
          ${checked ? 'bg-zinc-900 dark:bg-zinc-100' : 'bg-zinc-200 dark:bg-zinc-700'}
          ${className}
        `}
      >
        <span
          className={`
            pointer-events-none block h-4 w-4 rounded-full shadow-sm transition-transform
            ${checked ? 'translate-x-4 bg-white dark:bg-zinc-900' : 'translate-x-0 bg-white dark:bg-zinc-400'}
          `}
        />
      </button>
    )
  },
)

Switch.displayName = 'Switch'

export { Switch }
