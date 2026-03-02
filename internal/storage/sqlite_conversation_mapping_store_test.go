package storage

import (
	"testing"
)

// createTestSession is a helper that creates a chat session and returns its ID.
func createTestSession(t *testing.T, store *SQLiteChatStore) string {
	t.Helper()
	session, err := store.CreateSession("", "", "", "")
	if err != nil {
		t.Fatalf("creating test session: %v", err)
	}
	return session.ID
}

func TestSQLiteConversationMappingStore_CreateAndGet(t *testing.T) {
	db := newTestDB(t)
	chatStore := NewSQLiteChatStore(db)
	store := NewSQLiteConversationMappingStore(db)

	sessionID := createTestSession(t, chatStore)

	mapping := &ConversationMapping{
		SessionID:    sessionID,
		PlatformType: "telegram",
		PlatformID:   "integration-1",
		ChannelID:    "12345",
		AgentSlug:    "test-agent",
	}

	if err := store.Create(mapping); err != nil {
		t.Fatalf("Create failed: %v", err)
	}

	if mapping.ID == "" {
		t.Error("expected ID to be set after Create")
	}
	if mapping.CreatedAt.IsZero() {
		t.Error("expected CreatedAt to be set")
	}

	// Get by platform channel.
	got, err := store.GetByPlatformChannel("telegram", "integration-1", "12345")
	if err != nil {
		t.Fatalf("GetByPlatformChannel failed: %v", err)
	}
	if got == nil {
		t.Fatal("expected mapping, got nil")
	}
	if got.SessionID != sessionID {
		t.Errorf("got session ID %q, want %q", got.SessionID, sessionID)
	}
	if got.AgentSlug != "test-agent" {
		t.Errorf("got agent slug %q, want %q", got.AgentSlug, "test-agent")
	}

	// Get by session ID.
	got2, err := store.GetBySessionID(sessionID)
	if err != nil {
		t.Fatalf("GetBySessionID failed: %v", err)
	}
	if got2 == nil {
		t.Fatal("expected mapping, got nil")
	}
	if got2.ChannelID != "12345" {
		t.Errorf("got channel ID %q, want %q", got2.ChannelID, "12345")
	}
}

func TestSQLiteConversationMappingStore_NotFound(t *testing.T) {
	db := newTestDB(t)
	store := NewSQLiteConversationMappingStore(db)

	got, err := store.GetByPlatformChannel("telegram", "nonexistent", "999")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if got != nil {
		t.Error("expected nil for nonexistent mapping")
	}

	got2, err := store.GetBySessionID("nonexistent")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if got2 != nil {
		t.Error("expected nil for nonexistent session")
	}
}

func TestSQLiteConversationMappingStore_UpdateAgentSlug(t *testing.T) {
	db := newTestDB(t)
	chatStore := NewSQLiteChatStore(db)
	store := NewSQLiteConversationMappingStore(db)

	sessionID := createTestSession(t, chatStore)

	mapping := &ConversationMapping{
		SessionID:    sessionID,
		PlatformType: "telegram",
		PlatformID:   "integration-1",
		ChannelID:    "12345",
		AgentSlug:    "old-agent",
	}

	if err := store.Create(mapping); err != nil {
		t.Fatalf("Create failed: %v", err)
	}

	if err := store.UpdateAgentSlug(mapping.ID, "new-agent"); err != nil {
		t.Fatalf("UpdateAgentSlug failed: %v", err)
	}

	got, _ := store.GetByPlatformChannel("telegram", "integration-1", "12345")
	if got.AgentSlug != "new-agent" {
		t.Errorf("got agent slug %q, want %q", got.AgentSlug, "new-agent")
	}
}

func TestSQLiteConversationMappingStore_Delete(t *testing.T) {
	db := newTestDB(t)
	chatStore := NewSQLiteChatStore(db)
	store := NewSQLiteConversationMappingStore(db)

	sessionID := createTestSession(t, chatStore)

	mapping := &ConversationMapping{
		SessionID:    sessionID,
		PlatformType: "telegram",
		PlatformID:   "integration-1",
		ChannelID:    "12345",
	}

	if err := store.Create(mapping); err != nil {
		t.Fatalf("Create failed: %v", err)
	}

	if err := store.Delete(mapping.ID); err != nil {
		t.Fatalf("Delete failed: %v", err)
	}

	got, _ := store.GetByPlatformChannel("telegram", "integration-1", "12345")
	if got != nil {
		t.Error("expected nil after delete")
	}
}

func TestSQLiteConversationMappingStore_DeleteBySessionID(t *testing.T) {
	db := newTestDB(t)
	chatStore := NewSQLiteChatStore(db)
	store := NewSQLiteConversationMappingStore(db)

	sessionID := createTestSession(t, chatStore)

	mapping := &ConversationMapping{
		SessionID:    sessionID,
		PlatformType: "telegram",
		PlatformID:   "integration-1",
		ChannelID:    "12345",
	}

	if err := store.Create(mapping); err != nil {
		t.Fatalf("Create failed: %v", err)
	}

	if err := store.DeleteBySessionID(sessionID); err != nil {
		t.Fatalf("DeleteBySessionID failed: %v", err)
	}

	got, _ := store.GetBySessionID(sessionID)
	if got != nil {
		t.Error("expected nil after delete by session ID")
	}
}

func TestSQLiteConversationMappingStore_ListByPlatform(t *testing.T) {
	db := newTestDB(t)
	chatStore := NewSQLiteChatStore(db)
	store := NewSQLiteConversationMappingStore(db)

	// Create two mappings for the same platform.
	for _, ch := range []string{"111", "222"} {
		sid := createTestSession(t, chatStore)
		m := &ConversationMapping{
			SessionID:    sid,
			PlatformType: "telegram",
			PlatformID:   "integration-1",
			ChannelID:    ch,
		}
		if err := store.Create(m); err != nil {
			t.Fatalf("Create failed: %v", err)
		}
	}

	// Create one for a different platform.
	sid := createTestSession(t, chatStore)
	m := &ConversationMapping{
		SessionID:    sid,
		PlatformType: "slack",
		PlatformID:   "integration-2",
		ChannelID:    "C123",
	}
	if err := store.Create(m); err != nil {
		t.Fatalf("Create failed: %v", err)
	}

	list, err := store.ListByPlatform("telegram", "integration-1")
	if err != nil {
		t.Fatalf("ListByPlatform failed: %v", err)
	}
	if len(list) != 2 {
		t.Errorf("got %d mappings, want 2", len(list))
	}

	// Slack platform should only return 1.
	list2, err := store.ListByPlatform("slack", "integration-2")
	if err != nil {
		t.Fatalf("ListByPlatform failed: %v", err)
	}
	if len(list2) != 1 {
		t.Errorf("got %d mappings, want 1", len(list2))
	}
}

func TestSQLiteConversationMappingStore_UniqueConstraint(t *testing.T) {
	db := newTestDB(t)
	chatStore := NewSQLiteChatStore(db)
	store := NewSQLiteConversationMappingStore(db)

	sid1 := createTestSession(t, chatStore)
	sid2 := createTestSession(t, chatStore)

	mapping := &ConversationMapping{
		SessionID:    sid1,
		PlatformType: "telegram",
		PlatformID:   "integration-1",
		ChannelID:    "12345",
	}
	if err := store.Create(mapping); err != nil {
		t.Fatalf("Create failed: %v", err)
	}

	// Creating another mapping with same platform+channel should fail.
	dup := &ConversationMapping{
		SessionID:    sid2,
		PlatformType: "telegram",
		PlatformID:   "integration-1",
		ChannelID:    "12345",
	}
	if err := store.Create(dup); err == nil {
		t.Error("expected unique constraint violation")
	}
}

func TestSQLiteConversationMappingStore_CascadeDelete(t *testing.T) {
	db := newTestDB(t)
	chatStore := NewSQLiteChatStore(db)
	store := NewSQLiteConversationMappingStore(db)

	sessionID := createTestSession(t, chatStore)

	mapping := &ConversationMapping{
		SessionID:    sessionID,
		PlatformType: "telegram",
		PlatformID:   "integration-1",
		ChannelID:    "12345",
	}
	if err := store.Create(mapping); err != nil {
		t.Fatalf("Create failed: %v", err)
	}

	// Deleting the chat session should cascade-delete the mapping.
	if err := chatStore.DeleteSession(sessionID); err != nil {
		t.Fatalf("DeleteSession failed: %v", err)
	}

	got, err := store.GetByPlatformChannel("telegram", "integration-1", "12345")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if got != nil {
		t.Error("expected mapping to be cascade-deleted with session")
	}
}
