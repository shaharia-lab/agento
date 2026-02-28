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

// NotificationSettings represents the persisted notification configuration.
// The name is intentional: it provides clarity when referenced as notification.NotificationSettings.
//
//nolint:revive
type NotificationSettings struct {
	Enabled  bool       `json:"enabled"`
	Provider SMTPConfig `json:"provider"`
}
