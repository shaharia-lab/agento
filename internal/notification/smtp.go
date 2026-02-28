package notification

import (
	"context"
	"fmt"
	"strings"

	"github.com/wneessen/go-mail"
)

// SMTPProvider delivers notifications via SMTP using the go-mail library.
type SMTPProvider struct {
	config SMTPConfig
}

// NewSMTPProvider creates a new SMTPProvider with the given configuration.
func NewSMTPProvider(config SMTPConfig) *SMTPProvider {
	return &SMTPProvider{config: config}
}

// Name returns the provider identifier.
func (p *SMTPProvider) Name() string { return "smtp" }

// Send delivers msg using the configured SMTP server.
func (p *SMTPProvider) Send(ctx context.Context, msg Message) error {
	m := mail.NewMsg()
	if err := m.From(p.config.FromAddr); err != nil {
		return fmt.Errorf("invalid from address: %w", err)
	}

	recipients := strings.Split(p.config.ToAddrs, ",")
	for _, r := range recipients {
		r = strings.TrimSpace(r)
		if r == "" {
			continue
		}
		if err := m.To(r); err != nil {
			return fmt.Errorf("invalid recipient %q: %w", r, err)
		}
	}

	m.Subject(msg.Subject)

	// Plain-text fallback for clients that don't render HTML.
	m.SetBodyString(mail.TypeTextPlain, msg.Body)

	// Rich HTML email using the branded template.
	if html, err := buildEmailHTML(msg.Subject, msg.Body); err == nil {
		m.AddAlternativeString(mail.TypeTextHTML, html)
	}

	tlsPolicy := tlsPolicyFromEncryption(p.config.Encryption)

	c, err := mail.NewClient(p.config.Host,
		mail.WithPort(p.config.Port),
		mail.WithSMTPAuth(mail.SMTPAuthPlain),
		mail.WithUsername(p.config.Username),
		mail.WithPassword(p.config.Password),
		mail.WithTLSPolicy(tlsPolicy),
	)
	if err != nil {
		return fmt.Errorf("failed to create mail client: %w", err)
	}

	return c.DialAndSendWithContext(ctx, m)
}

// tlsPolicyFromEncryption converts the encryption string to a go-mail TLSPolicy.
func tlsPolicyFromEncryption(enc string) mail.TLSPolicy {
	switch enc {
	case "ssl_tls":
		return mail.TLSMandatory
	case "starttls":
		return mail.TLSOpportunistic
	default:
		return mail.NoTLS
	}
}
