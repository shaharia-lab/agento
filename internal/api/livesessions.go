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
type liveSessionStore struct {
	mu       sync.Mutex
	sessions map[string]*liveSession
}

func newLiveSessionStore() *liveSessionStore {
	return &liveSessionStore{sessions: make(map[string]*liveSession)}
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
