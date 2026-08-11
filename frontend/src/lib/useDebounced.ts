import { useEffect, useState } from 'react'

/**
 * The value, held back until it has stopped changing for `delayMs`.
 *
 * Used for the sessions search box, where every keystroke would otherwise
 * become a request and a full list re-render. The current value still drives
 * the input — only what is *acted on* is delayed — so typing stays immediate
 * while the work behind it happens once.
 */
export function useDebounced<T>(value: T, delayMs: number): T {
  const [settled, setSettled] = useState(value)

  useEffect(() => {
    const timer = setTimeout(() => setSettled(value), delayMs)
    return () => clearTimeout(timer)
  }, [value, delayMs])

  return settled
}
