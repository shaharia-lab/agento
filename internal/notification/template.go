package notification

import (
	"bytes"
	"html/template"
	"strings"
)

// SubjectPrefix is prepended to every outgoing notification subject.
const SubjectPrefix = "Agento Notification - "

// subjectPrefix is the unexported alias used within this package.
const subjectPrefix = SubjectPrefix

// emailTmpl is the HTML wrapper applied to every outgoing notification.
// All fields are auto-escaped by html/template.
var emailTmpl = template.Must(template.New("email").Parse(`<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width,initial-scale=1.0">
  <title>{{.FullSubject}}</title>
</head>
<body style="margin:0;padding:0;background-color:#f4f4f5;
     font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,Arial,sans-serif;">
  <table width="100%" cellpadding="0" cellspacing="0" role="presentation"
         style="background-color:#f4f4f5;padding:40px 16px;">
    <tr>
      <td align="center">
        <table width="600" cellpadding="0" cellspacing="0" role="presentation"
               style="max-width:600px;width:100%;border-radius:12px;
                      box-shadow:0 1px 3px rgba(0,0,0,.10);">

          <!-- ── Header ────────────────────────────────────────── -->
          <tr>
            <td style="background-color:#ffffff;padding:24px 32px;
                       border-radius:12px 12px 0 0;border-bottom:1px solid #e5e7eb;">
              <table width="100%" cellpadding="0" cellspacing="0" role="presentation">
                <tr>
                  <!-- Logo -->
                  <td style="vertical-align:middle;">
                    <table cellpadding="0" cellspacing="0" role="presentation">
                      <tr>
                        <td style="vertical-align:middle;padding-right:12px;">
                          <div style="width:38px;height:38px;background-color:#000000;
                                      border-radius:8px;text-align:center;line-height:38px;
                                      font-size:21px;font-weight:900;color:#ffffff;">A</div>
                        </td>
                        <td style="vertical-align:middle;">
                          <span style="font-size:17px;font-weight:700;
                                       color:#111827;letter-spacing:-0.3px;">Agento</span>
                          <span style="display:block;font-size:11px;
                                       color:#9ca3af;margin-top:1px;">
                            Your personal AI agent platform
                          </span>
                        </td>
                      </tr>
                    </table>
                  </td>
                  <!-- Badge -->
                  <td align="right" style="vertical-align:middle;">
                    <span style="font-size:10px;font-weight:600;letter-spacing:0.8px;
                                 color:#6b7280;background-color:#f3f4f6;
                                 padding:4px 10px;border-radius:20px;">
                      NOTIFICATION
                    </span>
                  </td>
                </tr>
              </table>
            </td>
          </tr>

          <!-- ── Title ─────────────────────────────────────────── -->
          <tr>
            <td style="background-color:#f9fafb;padding:20px 32px;
                       border-bottom:1px solid #e5e7eb;">
              <p style="margin:0;font-size:16px;font-weight:600;
                        color:#111827;">{{.Title}}</p>
            </td>
          </tr>

          <!-- ── Body ──────────────────────────────────────────── -->
          <tr>
            <td style="background-color:#ffffff;padding:32px;">
              <div style="font-size:14px;line-height:1.75;color:#374151;
                          white-space:pre-wrap;word-break:break-word;">{{.Body}}</div>
            </td>
          </tr>

          <!-- ── Footer ────────────────────────────────────────── -->
          <tr>
            <td style="background-color:#f9fafb;padding:18px 32px;
                       border-top:1px solid #e5e7eb;border-radius:0 0 12px 12px;">
              <p style="margin:0;font-size:12px;color:#9ca3af;">
                Automated notification from
                <a href="https://github.com/shaharia-lab/agento"
                   style="color:#6b7280;text-decoration:none;">Agento</a>.
                You are receiving this because notifications are enabled
                in your instance.
              </p>
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

// buildEmailHTML renders the HTML email template.
// The in-body title strips the prefix so it reads cleanly inside the email.
func buildEmailHTML(subject, body string) (string, error) {
	title := strings.TrimPrefix(subject, SubjectPrefix)
	var buf bytes.Buffer
	err := emailTmpl.Execute(&buf, struct {
		FullSubject string
		Title       string
		Body        string
	}{subject, title, body})
	if err != nil {
		return "", err
	}
	return buf.String(), nil
}
