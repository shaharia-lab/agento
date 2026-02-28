package notification

// SMTPConfig holds connection parameters for the SMTP provider.
type SMTPConfig struct {
	Host       string `json:"host"`
	Port       int    `json:"port"`
	Username   string `json:"username"`
	Password   string `json:"password"`
	FromAddr   string `json:"from_address"`
	ToAddrs    string `json:"to_addresses"`
	Encryption string `json:"encryption"` // "none", "starttls", "ssl_tls"
}

// ScheduledTasksPreferences controls notifications for scheduled task events.
// A nil pointer means "use the default", which is enabled (true).
type ScheduledTasksPreferences struct {
	// OnFinished, when nil or true, enables notifications on task completion.
	OnFinished *bool `json:"on_finished,omitempty"`
	// OnFailed, when nil or true, enables notifications on task failure.
	OnFailed *bool `json:"on_failed,omitempty"`
}

// IsOnFinishedEnabled returns true unless OnFinished is explicitly set to false.
func (p ScheduledTasksPreferences) IsOnFinishedEnabled() bool {
	return p.OnFinished == nil || *p.OnFinished
}

// IsOnFailedEnabled returns true unless OnFailed is explicitly set to false.
func (p ScheduledTasksPreferences) IsOnFailedEnabled() bool {
	return p.OnFailed == nil || *p.OnFailed
}

// NotificationPreferences holds per-event-category notification preferences.
// The name is intentional: it provides clarity when referenced as notification.NotificationPreferences.
//
//nolint:revive
type NotificationPreferences struct {
	ScheduledTasks ScheduledTasksPreferences `json:"scheduled_tasks"`
}

// NotificationSettings represents the persisted notification configuration.
// The name is intentional: it provides clarity when referenced as notification.NotificationSettings.
//
//nolint:revive
type NotificationSettings struct {
	Enabled     bool                    `json:"enabled"`
	Provider    SMTPConfig              `json:"provider"`
	Preferences NotificationPreferences `json:"preferences"`
}
