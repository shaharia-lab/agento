// Global vitest setup, wired via `test.setupFiles` in vite.config.ts.
//
// The DOM matchers (`toBeInTheDocument`, `toHaveTextContent`, ...) are registered
// once here rather than per file, and `cleanup` unmounts anything a test rendered
// so the next test starts against an empty document — Testing Library renders into
// a container appended to `document.body`, which jsdom keeps for the whole file.
import '@testing-library/jest-dom/vitest'
import { cleanup } from '@testing-library/react'
import { afterEach } from 'vitest'

afterEach(() => cleanup())
