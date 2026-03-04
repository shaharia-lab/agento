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
// It also tracks in-flight sends (inFlight) to prevent concurrent
// POST /chats/{id}/messages requests on the same chat session. A second
// request while the first is still streaming would read a stale SDKSession
// from the DB, causing the Claude CLI to start a fresh session instead of
// resuming the right one. tryLock returns false immediately in that case so
// the caller can return 409 Conflict rather than blocking indefinitely.
// Entries are removed from inFlight in delete(), which is called after
// streaming completes, keeping the map bounded to active sessions only.
type liveSessionStore struct {
	mu       sync.Mutex
	sessions map[string]*liveSession
	inFlight map[string]struct{}
}

func newLiveSessionStore() *liveSessionStore {
	return &liveSessionStore{
		sessions: make(map[string]*liveSession),
		inFlight: make(map[string]struct{}),
	}
}

// tryLock marks the session as in-flight if it is not already busy.
// Returns an unlock function on success, or nil if the session already has a
// send in progress. The caller must invoke the returned function when the full
// send cycle (streaming + CommitMessage) completes.
func (s *liveSessionStore) tryLock(id string) func() {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, busy := s.inFlight[id]; busy {
		return nil
	}
	s.inFlight[id] = struct{}{}
	return func() {
		s.mu.Lock()
		delete(s.inFlight, id)
		s.mu.Unlock()
	}
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
	delete(s.inFlight, id)
}
