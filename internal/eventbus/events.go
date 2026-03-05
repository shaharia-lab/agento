package eventbus

// Session lifecycle event type constants published when the scanner detects
// new or changed Claude Code session JSONL files.
const (
	EventSessionDiscovered = "claude.session.discovered"
	EventSessionUpdated    = "claude.session.updated"
)

// Payload keys used in session lifecycle events.
const (
	PayloadKeySessionID   = "session_id"
	PayloadKeyFilePath    = "file_path"
	PayloadKeyProjectPath = "project_path"
)
