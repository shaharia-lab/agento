package storage

import (
	"bufio"
	"crypto/rand"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

// ChatSession represents a chat session's metadata.
type ChatSession struct {
	ID         string    `json:"id"`
	Title      string    `json:"title"`
	AgentSlug  string    `json:"agent_slug"`
	SDKSession string    `json:"sdk_session_id"`
	CreatedAt  time.Time `json:"created_at"`
	UpdatedAt  time.Time `json:"updated_at"`
}

// ChatMessage represents a single message in a chat session.
type ChatMessage struct {
	Role      string    `json:"role"`
	Content   string    `json:"content"`
	Timestamp time.Time `json:"timestamp"`
}

// ChatStore defines the interface for chat session persistence.
type ChatStore interface {
	ListSessions() ([]*ChatSession, error)
	GetSession(id string) (*ChatSession, error)
	GetSessionWithMessages(id string) (*ChatSession, []ChatMessage, error)
	CreateSession(agentSlug string) (*ChatSession, error)
	AppendMessage(sessionID string, msg ChatMessage) error
	UpdateSession(session *ChatSession) error
	DeleteSession(id string) error
}

// FSChatStore implements ChatStore on the local filesystem.
// Each session is a JSONL file: <dir>/<uuid>.jsonl
// First line: session metadata. Subsequent lines: messages.
type FSChatStore struct {
	dir string
}

// NewFSChatStore creates an FSChatStore rooted at dir.
func NewFSChatStore(dir string) *FSChatStore {
	return &FSChatStore{dir: dir}
}

// jsonlRecord is the on-disk representation for both session metadata and messages.
type jsonlRecord struct {
	Type string `json:"type"`
	// session fields
	ID         string    `json:"id,omitempty"`
	Title      string    `json:"title,omitempty"`
	AgentSlug  string    `json:"agent_slug,omitempty"`
	SDKSession string    `json:"sdk_session_id,omitempty"`
	CreatedAt  time.Time `json:"created_at,omitempty"`
	UpdatedAt  time.Time `json:"updated_at,omitempty"`
	// message fields
	Role      string    `json:"role,omitempty"`
	Content   string    `json:"content,omitempty"`
	Timestamp time.Time `json:"timestamp,omitempty"`
}

func (s *FSChatStore) sessionPath(id string) string {
	return filepath.Join(s.dir, id+".jsonl")
}

func newUUID() string {
	var b [16]byte
	_, _ = rand.Read(b[:])
	b[6] = (b[6] & 0x0f) | 0x40 // version 4
	b[8] = (b[8] & 0x3f) | 0x80 // variant bits
	return fmt.Sprintf("%08x-%04x-%04x-%04x-%012x",
		b[0:4], b[4:6], b[6:8], b[8:10], b[10:])
}

// ListSessions returns all chat sessions ordered by most recently updated.
func (s *FSChatStore) ListSessions() ([]*ChatSession, error) {
	entries, err := os.ReadDir(s.dir)
	if err != nil {
		if os.IsNotExist(err) {
			return []*ChatSession{}, nil
		}
		return nil, fmt.Errorf("reading chats dir: %w", err)
	}

	sessions := make([]*ChatSession, 0, len(entries))
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".jsonl") {
			continue
		}
		id := strings.TrimSuffix(entry.Name(), ".jsonl")
		session, err := s.GetSession(id)
		if err != nil || session == nil {
			continue
		}
		sessions = append(sessions, session)
	}
	sort.Slice(sessions, func(i, j int) bool {
		return sessions[i].UpdatedAt.After(sessions[j].UpdatedAt)
	})
	if sessions == nil {
		sessions = []*ChatSession{}
	}
	return sessions, nil
}

// GetSession returns session metadata for the given ID, or nil if not found.
func (s *FSChatStore) GetSession(id string) (*ChatSession, error) {
	f, err := os.Open(s.sessionPath(id))
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, err
	}
	defer func() { _ = f.Close() }()

	scanner := bufio.NewScanner(f)
	if !scanner.Scan() {
		return nil, fmt.Errorf("empty session file for %q", id)
	}

	var rec jsonlRecord
	if err := json.Unmarshal(scanner.Bytes(), &rec); err != nil {
		return nil, fmt.Errorf("parsing session record: %w", err)
	}
	if rec.Type != "session" {
		return nil, fmt.Errorf("invalid session file: first record type is %q", rec.Type)
	}
	return &ChatSession{
		ID:         rec.ID,
		Title:      rec.Title,
		AgentSlug:  rec.AgentSlug,
		SDKSession: rec.SDKSession,
		CreatedAt:  rec.CreatedAt,
		UpdatedAt:  rec.UpdatedAt,
	}, nil
}

// GetSessionWithMessages returns the session and its full message history.
func (s *FSChatStore) GetSessionWithMessages(id string) (*ChatSession, []ChatMessage, error) {
	f, err := os.Open(s.sessionPath(id))
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil, nil
		}
		return nil, nil, err
	}
	defer func() { _ = f.Close() }()

	var session *ChatSession
	var messages []ChatMessage

	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 1024*1024), 1024*1024)

	first := true
	for scanner.Scan() {
		var rec jsonlRecord
		if err := json.Unmarshal(scanner.Bytes(), &rec); err != nil {
			continue
		}
		if first {
			first = false
			if rec.Type == "session" {
				session = &ChatSession{
					ID:         rec.ID,
					Title:      rec.Title,
					AgentSlug:  rec.AgentSlug,
					SDKSession: rec.SDKSession,
					CreatedAt:  rec.CreatedAt,
					UpdatedAt:  rec.UpdatedAt,
				}
			}
			continue
		}
		if rec.Type == "message" {
			messages = append(messages, ChatMessage{
				Role:      rec.Role,
				Content:   rec.Content,
				Timestamp: rec.Timestamp,
			})
		}
	}
	if messages == nil {
		messages = []ChatMessage{}
	}
	return session, messages, scanner.Err()
}

// CreateSession creates a new chat session with the given agent slug (may be empty).
func (s *FSChatStore) CreateSession(agentSlug string) (*ChatSession, error) {
	id := newUUID()
	now := time.Now().UTC()
	session := &ChatSession{
		ID:        id,
		Title:     "New Chat",
		AgentSlug: agentSlug,
		CreatedAt: now,
		UpdatedAt: now,
	}
	rec := jsonlRecord{
		Type:      "session",
		ID:        id,
		Title:     session.Title,
		AgentSlug: agentSlug,
		CreatedAt: now,
		UpdatedAt: now,
	}
	data, err := json.Marshal(rec)
	if err != nil {
		return nil, err
	}
	path := s.sessionPath(id)
	if err := os.WriteFile(path, append(data, '\n'), 0600); err != nil {
		return nil, fmt.Errorf("creating session file: %w", err)
	}
	return session, nil
}

// AppendMessage appends a message to the session's JSONL file.
func (s *FSChatStore) AppendMessage(sessionID string, msg ChatMessage) error {
	rec := jsonlRecord{
		Type:      "message",
		Role:      msg.Role,
		Content:   msg.Content,
		Timestamp: msg.Timestamp,
	}
	data, err := json.Marshal(rec)
	if err != nil {
		return err
	}
	f, err := os.OpenFile(s.sessionPath(sessionID), os.O_APPEND|os.O_WRONLY, 0600)
	if err != nil {
		return fmt.Errorf("opening session for append: %w", err)
	}
	defer func() { _ = f.Close() }()
	_, err = f.Write(append(data, '\n'))
	return err
}

// UpdateSession rewrites the session metadata (first line) in the JSONL file.
func (s *FSChatStore) UpdateSession(session *ChatSession) error {
	path := s.sessionPath(session.ID)
	data, err := os.ReadFile(path) //nolint:gosec // path constructed from admin-configured data dir
	if err != nil {
		return fmt.Errorf("reading session file: %w", err)
	}

	content := strings.TrimRight(string(data), "\n")
	lines := strings.Split(content, "\n")
	if len(lines) == 0 {
		return fmt.Errorf("empty session file")
	}

	rec := jsonlRecord{
		Type:       "session",
		ID:         session.ID,
		Title:      session.Title,
		AgentSlug:  session.AgentSlug,
		SDKSession: session.SDKSession,
		CreatedAt:  session.CreatedAt,
		UpdatedAt:  session.UpdatedAt,
	}
	firstLine, err := json.Marshal(rec)
	if err != nil {
		return err
	}
	lines[0] = string(firstLine)
	return os.WriteFile(path, []byte(strings.Join(lines, "\n")+"\n"), 0600)
}

// DeleteSession removes the JSONL file for the given session ID.
func (s *FSChatStore) DeleteSession(id string) error {
	path := s.sessionPath(id)
	if err := os.Remove(path); err != nil {
		if os.IsNotExist(err) {
			return fmt.Errorf("session %q not found", id)
		}
		return fmt.Errorf("deleting session %q: %w", id, err)
	}
	return nil
}
