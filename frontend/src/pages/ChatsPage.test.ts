import { describe, it, expect } from 'vitest'
import type { ChatSession } from '@/types'

/**
 * Pure filter logic extracted from ChatsPage's `filtered` useMemo.
 * This mirrors the exact conditions used in ChatsPage.tsx.
 */
function filterSessions(
  sessions: ChatSession[],
  search: string,
  filterAgent: string,
  filterWorkingDir: string,
): ChatSession[] {
  return sessions.filter(s => {
    const matchesSearch = !search || s.title.toLowerCase().includes(search.toLowerCase())
    const matchesAgent = filterAgent === 'all' || s.agent_slug === filterAgent
    const matchesWorkingDir = filterWorkingDir === 'all' || s.working_directory === filterWorkingDir
    return matchesSearch && matchesAgent && matchesWorkingDir
  })
}

/**
 * Derives unique working directories from sessions, matching ChatsPage logic.
 */
function uniqueWorkingDirs(sessions: ChatSession[]): string[] {
  return [...new Set(sessions.map(s => s.working_directory).filter(Boolean))].sort()
}

function makeSession(overrides: Partial<ChatSession> = {}): ChatSession {
  return {
    id: 'id-1',
    title: 'Test Chat',
    agent_slug: '',
    sdk_session_id: '',
    working_directory: '',
    model: 'claude-sonnet-4-6',
    created_at: '2026-01-01T00:00:00Z',
    updated_at: '2026-01-01T00:00:00Z',
    ...overrides,
  }
}

describe('ChatsPage filter logic', () => {
  const sessions: ChatSession[] = [
    makeSession({
      id: '1',
      title: 'Fix bug',
      agent_slug: 'dev',
      working_directory: '/home/user/project-a',
    }),
    makeSession({
      id: '2',
      title: 'Write docs',
      agent_slug: 'writer',
      working_directory: '/home/user/project-b',
    }),
    makeSession({
      id: '3',
      title: 'Debug issue',
      agent_slug: 'dev',
      working_directory: '/home/user/project-a',
    }),
    makeSession({
      id: '4',
      title: 'Review PR',
      agent_slug: 'reviewer',
      working_directory: '/home/user/project-b',
    }),
    makeSession({ id: '5', title: 'Quick question', agent_slug: '', working_directory: '' }),
  ]

  describe('filterSessions', () => {
    it('returns all sessions when no filters are active', () => {
      const result = filterSessions(sessions, '', 'all', 'all')
      expect(result).toHaveLength(5)
    })

    it('filters by search text (case-insensitive)', () => {
      const result = filterSessions(sessions, 'fix', 'all', 'all')
      expect(result).toHaveLength(1)
      expect(result[0].id).toBe('1')
    })

    it('filters by agent', () => {
      const result = filterSessions(sessions, '', 'dev', 'all')
      expect(result).toHaveLength(2)
      expect(result.map(s => s.id)).toEqual(['1', '3'])
    })

    it('filters by working directory', () => {
      const result = filterSessions(sessions, '', 'all', '/home/user/project-a')
      expect(result).toHaveLength(2)
      expect(result.map(s => s.id)).toEqual(['1', '3'])
    })

    it('combines search and working directory filters', () => {
      const result = filterSessions(sessions, 'debug', 'all', '/home/user/project-a')
      expect(result).toHaveLength(1)
      expect(result[0].id).toBe('3')
    })

    it('combines agent and working directory filters', () => {
      const result = filterSessions(sessions, '', 'dev', '/home/user/project-a')
      expect(result).toHaveLength(2)
    })

    it('combines all three filters', () => {
      const result = filterSessions(sessions, 'fix', 'dev', '/home/user/project-a')
      expect(result).toHaveLength(1)
      expect(result[0].id).toBe('1')
    })

    it('returns empty when no sessions match', () => {
      const result = filterSessions(sessions, 'nonexistent', 'all', 'all')
      expect(result).toHaveLength(0)
    })

    it('sessions with empty working_directory do not match a specific directory filter', () => {
      const result = filterSessions(sessions, '', 'all', '/home/user/project-a')
      // Session 5 has empty working_directory, should not appear
      expect(result.find(s => s.id === '5')).toBeUndefined()
    })

    it('sessions with empty working_directory appear when filter is all', () => {
      const result = filterSessions(sessions, '', 'all', 'all')
      expect(result.find(s => s.id === '5')).toBeDefined()
    })
  })

  describe('uniqueWorkingDirs', () => {
    it('returns unique directories sorted alphabetically', () => {
      const dirs = uniqueWorkingDirs(sessions)
      expect(dirs).toEqual(['/home/user/project-a', '/home/user/project-b'])
    })

    it('excludes empty working directories', () => {
      const dirs = uniqueWorkingDirs(sessions)
      expect(dirs).not.toContain('')
    })

    it('returns empty array when no sessions have working directories', () => {
      const emptySessions = [
        makeSession({ id: '1', working_directory: '' }),
        makeSession({ id: '2', working_directory: '' }),
      ]
      expect(uniqueWorkingDirs(emptySessions)).toEqual([])
    })

    it('returns empty array for empty sessions list', () => {
      expect(uniqueWorkingDirs([])).toEqual([])
    })

    it('deduplicates directories', () => {
      const dupeSessions = [
        makeSession({ id: '1', working_directory: '/home/user/a' }),
        makeSession({ id: '2', working_directory: '/home/user/a' }),
        makeSession({ id: '3', working_directory: '/home/user/b' }),
      ]
      expect(uniqueWorkingDirs(dupeSessions)).toEqual(['/home/user/a', '/home/user/b'])
    })
  })
})
