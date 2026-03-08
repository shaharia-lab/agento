package whatsapp

import (
	"context"
	"encoding/json"
	"fmt"
	"time"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/proto/waE2E"
	"go.mau.fi/whatsmeow/types"
	"google.golang.org/protobuf/proto"
)

// textResult wraps a string in an MCP CallToolResult.
func textResult(text string) (*mcp.CallToolResult, any, error) {
	return &mcp.CallToolResult{
		Content: []mcp.Content{
			&mcp.TextContent{Text: text},
		},
	}, nil, nil
}

// registerMessagingTools adds all WhatsApp messaging MCP tools to the server.
// Only tools whose names are in the allowed set are registered.
// If allowed is empty, all tools are registered.
func registerMessagingTools(server *mcp.Server, client *Client, allowed map[string]bool) {
	if len(allowed) == 0 || allowed["send_message"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "send_message",
			Description: "Send a text message to a WhatsApp phone number or group JID.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *sendMessageParams) (*mcp.CallToolResult, any, error) {
			return handleSendMessage(ctx, client, params)
		})
	}

	if len(allowed) == 0 || allowed["send_media"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "send_media",
			Description: "Send an image or document to a WhatsApp phone number or group JID by URL.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *sendMediaParams) (*mcp.CallToolResult, any, error) {
			return handleSendMedia(ctx, client, params)
		})
	}

	if len(allowed) == 0 || allowed["list_chats"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "list_chats",
			Description: "List WhatsApp chats from the device's contact store.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *listChatsParams) (*mcp.CallToolResult, any, error) {
			return handleListChats(ctx, client, params)
		})
	}

	if len(allowed) == 0 || allowed["get_contacts"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "get_contacts",
			Description: "List contacts from the linked WhatsApp device.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *getContactsParams) (*mcp.CallToolResult, any, error) {
			return handleGetContacts(ctx, client, params)
		})
	}
}

// ── Parameter types and handlers ─────────────────────────────────────────────

// parseJID parses a JID string. If the input looks like a phone number (digits only),
// it is treated as an individual chat JID (user@s.whatsapp.net).
func parseJID(jidStr string) (types.JID, error) {
	// Try direct parse first.
	jid, err := types.ParseJID(jidStr)
	if err == nil {
		return jid, nil
	}
	// Treat as phone number: digits and '+' allowed -> user@s.whatsapp.net
	cleaned := ""
	for _, c := range jidStr {
		switch {
		case c >= '0' && c <= '9':
			cleaned += string(c)
		case c == '+':
			continue // skip plus sign
		default:
			return types.JID{}, fmt.Errorf("invalid JID or phone number: %s", jidStr)
		}
	}
	if cleaned == "" {
		return types.JID{}, fmt.Errorf("empty JID or phone number")
	}
	return types.NewJID(cleaned, types.DefaultUserServer), nil
}

// send_message

type sendMessageParams struct {
	To   string `json:"to" jsonschema:"required,Phone number (with country code) or group JID to send to"`
	Text string `json:"text" jsonschema:"required,Text content of the message"`
}

func handleSendMessage(
	ctx context.Context, client *Client, params *sendMessageParams,
) (*mcp.CallToolResult, any, error) {
	jid, err := parseJID(params.To)
	if err != nil {
		return nil, nil, fmt.Errorf("parsing recipient JID: %w", err)
	}

	msg := &waE2E.Message{
		Conversation: proto.String(params.Text),
	}

	resp, err := client.WM().SendMessage(ctx, jid, msg)
	if err != nil {
		return nil, nil, fmt.Errorf("sending message: %w", err)
	}

	return textResult(fmt.Sprintf(
		"Message sent successfully. ID: %s, Timestamp: %s",
		resp.ID, resp.Timestamp.Format(time.RFC3339),
	))
}

// send_media

type sendMediaParams struct {
	To       string `json:"to" jsonschema:"required,Phone number (with country code) or group JID to send to"`
	URL      string `json:"url" jsonschema:"required,URL of the image or document to send"`
	Caption  string `json:"caption" jsonschema:"Optional caption for the media"`
	Filename string `json:"filename" jsonschema:"Filename for document type (if set sends as document instead of image)"`
}

func handleSendMedia(
	ctx context.Context, client *Client, params *sendMediaParams,
) (*mcp.CallToolResult, any, error) {
	jid, err := parseJID(params.To)
	if err != nil {
		return nil, nil, fmt.Errorf("parsing recipient JID: %w", err)
	}

	// Download the media from URL.
	mediaData, err := downloadMedia(ctx, params.URL)
	if err != nil {
		return nil, nil, fmt.Errorf("downloading media: %w", err)
	}

	// Upload to WhatsApp servers.
	var msg *waE2E.Message
	if params.Filename != "" {
		// Send as document.
		uploaded, uploadErr := client.WM().Upload(ctx, mediaData, whatsmeow.MediaDocument)
		if uploadErr != nil {
			return nil, nil, fmt.Errorf("uploading document: %w", uploadErr)
		}
		msg = &waE2E.Message{
			DocumentMessage: &waE2E.DocumentMessage{
				URL:           proto.String(uploaded.URL),
				DirectPath:    proto.String(uploaded.DirectPath),
				MediaKey:      uploaded.MediaKey,
				FileEncSHA256: uploaded.FileEncSHA256,
				FileSHA256:    uploaded.FileSHA256,
				FileLength:    proto.Uint64(uploaded.FileLength),
				FileName:      proto.String(params.Filename),
				Caption:       proto.String(params.Caption),
				Mimetype:      proto.String(detectMIMEType(mediaData)),
			},
		}
	} else {
		// Send as image.
		uploaded, uploadErr := client.WM().Upload(ctx, mediaData, whatsmeow.MediaImage)
		if uploadErr != nil {
			return nil, nil, fmt.Errorf("uploading image: %w", uploadErr)
		}
		msg = &waE2E.Message{
			ImageMessage: &waE2E.ImageMessage{
				URL:           proto.String(uploaded.URL),
				DirectPath:    proto.String(uploaded.DirectPath),
				MediaKey:      uploaded.MediaKey,
				FileEncSHA256: uploaded.FileEncSHA256,
				FileSHA256:    uploaded.FileSHA256,
				FileLength:    proto.Uint64(uploaded.FileLength),
				Caption:       proto.String(params.Caption),
				Mimetype:      proto.String(detectMIMEType(mediaData)),
			},
		}
	}

	resp, err := client.WM().SendMessage(ctx, jid, msg)
	if err != nil {
		return nil, nil, fmt.Errorf("sending media: %w", err)
	}

	return textResult(fmt.Sprintf(
		"Media sent successfully. ID: %s, Timestamp: %s",
		resp.ID, resp.Timestamp.Format(time.RFC3339),
	))
}

// list_chats

type listChatsParams struct {
	Limit int `json:"limit" jsonschema:"Maximum number of entries to return (default 50, max 200)"`
}

func handleListChats(
	ctx context.Context, client *Client, params *listChatsParams,
) (*mcp.CallToolResult, any, error) {
	limit := params.Limit
	if limit <= 0 {
		limit = 50
	}
	if limit > 200 {
		limit = 200
	}

	// Use contacts as a proxy for chats — the device store tracks contacts
	// that have been communicated with.
	contacts, err := client.WM().Store.Contacts.GetAllContacts(ctx)
	if err != nil {
		return nil, nil, fmt.Errorf("listing contacts for chats: %w", err)
	}

	type chatEntry struct {
		JID      string `json:"jid"`
		Name     string `json:"name,omitempty"`
		PushName string `json:"push_name,omitempty"`
	}

	result := make([]chatEntry, 0, limit)
	count := 0
	for jid, contact := range contacts {
		if count >= limit {
			break
		}
		name := contact.FullName
		if name == "" {
			name = contact.BusinessName
		}
		result = append(result, chatEntry{
			JID:      jid.String(),
			Name:     name,
			PushName: contact.PushName,
		})
		count++
	}

	b, err := json.Marshal(result)
	if err != nil {
		return nil, nil, fmt.Errorf("marshaling chats: %w", err)
	}

	return textResult(fmt.Sprintf("Chats (%d): %s", len(result), string(b)))
}

// get_contacts

type getContactsParams struct {
	Limit int `json:"limit" jsonschema:"Maximum number of contacts to return (default 50, max 500)"`
}

func handleGetContacts(
	ctx context.Context, client *Client, params *getContactsParams,
) (*mcp.CallToolResult, any, error) {
	limit := params.Limit
	if limit <= 0 {
		limit = 50
	}
	if limit > 500 {
		limit = 500
	}

	contacts, err := client.WM().Store.Contacts.GetAllContacts(ctx)
	if err != nil {
		return nil, nil, fmt.Errorf("getting contacts: %w", err)
	}

	type contactInfo struct {
		JID          string `json:"jid"`
		PushName     string `json:"push_name,omitempty"`
		FullName     string `json:"full_name,omitempty"`
		FirstName    string `json:"first_name,omitempty"`
		BusinessName string `json:"business_name,omitempty"`
	}

	result := make([]contactInfo, 0, limit)
	count := 0
	for jid, contact := range contacts {
		if count >= limit {
			break
		}
		result = append(result, contactInfo{
			JID:          jid.String(),
			PushName:     contact.PushName,
			FullName:     contact.FullName,
			FirstName:    contact.FirstName,
			BusinessName: contact.BusinessName,
		})
		count++
	}

	b, err := json.Marshal(result)
	if err != nil {
		return nil, nil, fmt.Errorf("marshaling contacts: %w", err)
	}

	return textResult(fmt.Sprintf("Contacts (%d): %s", len(result), string(b)))
}
