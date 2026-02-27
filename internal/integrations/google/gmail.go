package google

import (
	"context"
	"encoding/base64"
	"fmt"
	"net/http"
	"strings"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
	"google.golang.org/api/gmail/v1"
	"google.golang.org/api/option"
)

// registerGmailTools adds Gmail MCP tools to the server.
func registerGmailTools(server *mcp.Server, httpClient *http.Client) {
	gmailSvc, err := gmail.NewService(context.Background(), option.WithHTTPClient(httpClient))
	if err != nil {
		return
	}

	mcp.AddTool(server, &mcp.Tool{
		Name:        "send_email",
		Description: "Sends an email via Gmail.",
	}, func(ctx context.Context, _ *mcp.CallToolRequest, params *sendEmailParams) (*mcp.CallToolResult, any, error) {
		return handleSendEmail(ctx, gmailSvc, params)
	})

	mcp.AddTool(server, &mcp.Tool{
		Name:        "read_email",
		Description: "Reads the full content of a Gmail message by its ID.",
	}, func(ctx context.Context, _ *mcp.CallToolRequest, params *readEmailParams) (*mcp.CallToolResult, any, error) {
		return handleReadEmail(ctx, gmailSvc, params)
	})

	mcp.AddTool(server, &mcp.Tool{
		Name:        "search_email",
		Description: "Searches Gmail messages using Gmail query syntax (e.g. 'from:alice@example.com is:unread').",
	}, func(ctx context.Context, _ *mcp.CallToolRequest, params *searchEmailParams) (*mcp.CallToolResult, any, error) {
		return handleSearchEmail(ctx, gmailSvc, params)
	})
}

type sendEmailParams struct {
	To      string `json:"to" jsonschema:"required,Recipient email address(es), comma-separated"`
	Subject string `json:"subject" jsonschema:"required,Email subject line"`
	Body    string `json:"body" jsonschema:"required,Plain text body of the email"`
}

func handleSendEmail(ctx context.Context, svc *gmail.Service, params *sendEmailParams) (*mcp.CallToolResult, any, error) {
	raw := fmt.Sprintf("To: %s\r\nSubject: %s\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n%s",
		params.To, params.Subject, params.Body)

	encoded := base64.URLEncoding.EncodeToString([]byte(raw))
	msg := &gmail.Message{Raw: encoded}

	sent, err := svc.Users.Messages.Send("me", msg).Context(ctx).Do()
	if err != nil {
		return nil, nil, fmt.Errorf("sending email: %w", err)
	}

	return &mcp.CallToolResult{
		Content: []mcp.Content{
			&mcp.TextContent{
				Text: fmt.Sprintf("Email sent successfully. Message ID: %s", sent.Id),
			},
		},
	}, nil, nil
}

type readEmailParams struct {
	MessageID string `json:"message_id" jsonschema:"required,The Gmail message ID to read"`
}

func handleReadEmail(ctx context.Context, svc *gmail.Service, params *readEmailParams) (*mcp.CallToolResult, any, error) {
	msg, err := svc.Users.Messages.Get("me", params.MessageID).
		Format("full").
		Context(ctx).
		Do()
	if err != nil {
		return nil, nil, fmt.Errorf("reading email %q: %w", params.MessageID, err)
	}

	var subject, from, date, body string
	for _, h := range msg.Payload.Headers {
		switch h.Name {
		case "Subject":
			subject = h.Value
		case "From":
			from = h.Value
		case "Date":
			date = h.Value
		}
	}

	body = extractBody(msg.Payload)

	result := fmt.Sprintf("Subject: %s\nFrom: %s\nDate: %s\n\n%s", subject, from, date, body)
	return &mcp.CallToolResult{
		Content: []mcp.Content{&mcp.TextContent{Text: result}},
	}, nil, nil
}

// extractBody recursively extracts plain-text body from a message payload.
func extractBody(payload *gmail.MessagePart) string {
	if payload == nil {
		return ""
	}

	if payload.MimeType == "text/plain" && payload.Body != nil && payload.Body.Data != "" {
		decoded, err := base64.URLEncoding.DecodeString(payload.Body.Data)
		if err == nil {
			return string(decoded)
		}
	}

	for _, part := range payload.Parts {
		if text := extractBody(part); text != "" {
			return text
		}
	}
	return ""
}

type searchEmailParams struct {
	Query      string `json:"query" jsonschema:"required,Gmail search query (e.g. 'from:alice@example.com is:unread')"`
	MaxResults int64  `json:"max_results" jsonschema:"Maximum number of messages to return (default 10, max 50)"`
}

func handleSearchEmail(ctx context.Context, svc *gmail.Service, params *searchEmailParams) (*mcp.CallToolResult, any, error) {
	maxResults := params.MaxResults
	if maxResults <= 0 || maxResults > 50 {
		maxResults = 10
	}

	list, err := svc.Users.Messages.List("me").
		Q(params.Query).
		MaxResults(maxResults).
		Context(ctx).
		Do()
	if err != nil {
		return nil, nil, fmt.Errorf("searching emails: %w", err)
	}

	if len(list.Messages) == 0 {
		return &mcp.CallToolResult{
			Content: []mcp.Content{&mcp.TextContent{Text: "No messages found matching the query."}},
		}, nil, nil
	}

	results := make([]string, 0, len(list.Messages))
	for _, m := range list.Messages {
		msg, err := svc.Users.Messages.Get("me", m.Id).
			Format("metadata").
			MetadataHeaders("Subject", "From", "Date").
			Context(ctx).
			Do()
		if err != nil {
			continue
		}
		var subject, from, date string
		for _, h := range msg.Payload.Headers {
			switch h.Name {
			case "Subject":
				subject = h.Value
			case "From":
				from = h.Value
			case "Date":
				date = h.Value
			}
		}
		results = append(results, fmt.Sprintf("ID: %s\nSubject: %s\nFrom: %s\nDate: %s", m.Id, subject, from, date))
	}

	return &mcp.CallToolResult{
		Content: []mcp.Content{&mcp.TextContent{
			Text: fmt.Sprintf("Found %d message(s):\n\n%s", len(list.Messages), strings.Join(results, "\n\n---\n\n")),
		}},
	}, nil, nil
}
