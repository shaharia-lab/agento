package api

import (
	"sync"

	claude "github.com/shaharia-lab/claude-agent-sdk-go/claude"
)

// liveSession holds a running SDK session and channels the SSE handler blocks
// on while waiting for the user to answer an AskUserQuestion prompt or approve
// a tool permission request.
type liveSession struct {
	session          *claude.Session
	inputCh          chan string
	permissionRespCh chan bool
}

// liveSessionStore is an in-memory map of chat-session-ID → liveSession.
// It is safe for concurrent use.
//
// It also holds a per-session mutex (inFlight) so that concurrent
// POST /chats/{id}/messages requests are serialized per chat session.
// Without this, a second request arriving while the first is still streaming
// reads a stale SDKSession from the DB (empty or the previous value), causing
// the Claude CLI to start a fresh session instead of resuming the right one.
type liveSessionStore struct {
	mu       sync.Mutex
	sessions map[string]*liveSession
	inFlight map[string]*sync.Mutex
}

func newLiveSessionStore() *liveSessionStore {
	return &liveSessionStore{
		sessions: make(map[string]*liveSession),
		inFlight: make(map[string]*sync.Mutex),
	}
}

// lock acquires the per-session send mutex. The caller must call the returned
// function to release it when the send (including CommitMessage) is complete.
func (s *liveSessionStore) lock(id string) func() {
	s.mu.Lock()
	m, ok := s.inFlight[id]
	if !ok {
		m = &sync.Mutex{}
		s.inFlight[id] = m
	}
	s.mu.Unlock()
	m.Lock()
	return m.Unlock
}

func (s *liveSessionStore) put(id string, ls *liveSession) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.sessions[id] = ls
}

func (s *liveSessionStore) get(id string) (*liveSession, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	ls, ok := s.sessions[id]
	return ls, ok
}

func (s *liveSessionStore) delete(id string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.sessions, id)
}
