package notification

import (
	"bytes"
	"html/template"
)

// SubjectPrefix is prepended to every outgoing notification subject.
const SubjectPrefix = "Agento Notification - "

// subjectPrefix is the unexported alias used within this package.
const subjectPrefix = SubjectPrefix

// emailTmpl is the HTML wrapper applied to every outgoing notification.
// {{.Subject}} and {{.Body}} are auto-escaped by html/template.
var emailTmpl = template.Must(template.New("email").Parse(`<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width,initial-scale=1.0">
  <title>{{.Subject}}</title>
</head>
<body style="margin:0;padding:0;background-color:#f4f4f5;
     font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Arial,sans-serif;">
  <table width="100%" cellpadding="0" cellspacing="0" role="presentation"
         style="background-color:#f4f4f5;padding:40px 16px;">
    <tr>
      <td align="center">
        <table width="600" cellpadding="0" cellspacing="0" role="presentation"
               style="max-width:600px;width:100%;">

          <!-- ── Header ─────────────────────────────────────────── -->
          <tr>
            <td style="background-color:#0f0f1a;padding:28px 40px;border-radius:12px 12px 0 0;">
              <table width="100%" cellpadding="0" cellspacing="0" role="presentation">
                <tr>
                  <td style="vertical-align:middle;">
                    <!-- Logo mark -->
                    <table cellpadding="0" cellspacing="0" role="presentation">
                      <tr>
                        <td style="vertical-align:middle;padding-right:12px;">
                          <div style="width:36px;height:36px;background:linear-gradient(135deg,#6366f1,#8b5cf6);
                                      border-radius:8px;display:inline-block;text-align:center;line-height:36px;
                                      font-size:20px;font-weight:900;color:#ffffff;">A</div>
                        </td>
                        <td style="vertical-align:middle;">
                          <span style="font-size:20px;font-weight:700;
                                       color:#ffffff;letter-spacing:-0.3px;">Agento</span>
                          <span style="display:block;font-size:11px;color:#6b7280;margin-top:1px;letter-spacing:0.3px;">
                            AI Agent Platform
                          </span>
                        </td>
                      </tr>
                    </table>
                  </td>
                  <td align="right" style="vertical-align:middle;">
                    <span style="font-size:11px;color:#4b5563;background-color:#1e1e2e;
                                 padding:4px 10px;border-radius:20px;letter-spacing:0.4px;">
                      NOTIFICATION
                    </span>
                  </td>
                </tr>
              </table>
            </td>
          </tr>

          <!-- ── Subject bar ────────────────────────────────────── -->
          <tr>
            <td style="background-color:#18181f;padding:16px 40px;border-left:3px solid #6366f1;">
              <p style="margin:0;font-size:15px;font-weight:600;color:#e5e7eb;">{{.Subject}}</p>
            </td>
          </tr>

          <!-- ── Body ──────────────────────────────────────────── -->
          <tr>
            <td style="background-color:#ffffff;padding:36px 40px;">
              <div style="font-size:14px;line-height:1.7;color:#374151;
                          white-space:pre-wrap;word-break:break-word;">{{.Body}}</div>
            </td>
          </tr>

          <!-- ── Footer ────────────────────────────────────────── -->
          <tr>
            <td style="background-color:#f9fafb;padding:20px 40px;
                       border-top:1px solid #e5e7eb;border-radius:0 0 12px 12px;">
              <table width="100%" cellpadding="0" cellspacing="0" role="presentation">
                <tr>
                  <td>
                    <p style="margin:0;font-size:12px;color:#9ca3af;">
                      Automated notification from
                      <a href="https://github.com/shaharia-lab/agento"
                         style="color:#6366f1;text-decoration:none;">Agento</a>.
                      You are receiving this because notifications are enabled in your instance.
                    </p>
                  </td>
                </tr>
              </table>
            </td>
          </tr>

        </table>
      </td>
    </tr>
  </table>
</body>
</html>
`))

// buildSubject prepends the standard prefix to a subject line.
func buildSubject(subject string) string {
	return subjectPrefix + subject
}

// buildEmailHTML renders the HTML email template with the given subject and body.
func buildEmailHTML(subject, body string) (string, error) {
	var buf bytes.Buffer
	err := emailTmpl.Execute(&buf, struct{ Subject, Body string }{subject, body})
	if err != nil {
		return "", err
	}
	return buf.String(), nil
}
