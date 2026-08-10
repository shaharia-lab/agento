import { describe, it, expect } from 'vitest'
import { render, screen } from '@testing-library/react'
import { Badge } from './badge'

// Harness verification, not a Badge test suite. It exists so the component-test
// wiring cannot rot silently: the `.tsx` glob, the jsdom environment, the JSX
// transform, the `@/` alias (Badge imports `@/lib/utils`) and the jest-dom
// matchers registered in src/test/setup.ts are all exercised by this one render.
// Badge is deliberately the subject — no router, no API client, no context — so a
// failure here means the harness broke, not that a component needs providers.
describe('component test harness', () => {
  it('renders a component into the DOM and matches on its text', () => {
    render(<Badge>Scheduled</Badge>)

    expect(screen.getByText('Scheduled')).toBeInTheDocument()
  })

  it('applies variant classes, so the component actually ran rather than being stubbed', () => {
    render(<Badge variant="destructive">Failed</Badge>)

    expect(screen.getByText('Failed')).toHaveClass('bg-destructive')
  })
})
