package telegram

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
)

// apiURL builds the full Telegram Bot API endpoint URL.
func apiURL(token, method string) string {
	return fmt.Sprintf("https://api.telegram.org/bot%s/%s", token, method)
}

// callTelegram makes a POST request to the Telegram Bot API and returns the parsed response.
func callTelegram(ctx context.Context, token, method string, payload any) (*telegramResponse, error) {
	body, err := json.Marshal(payload)
	if err != nil {
		return nil, fmt.Errorf("marshaling request: %w", err)
	}

	req, err := http.NewRequestWithContext(
		ctx, http.MethodPost, apiURL(token, method), bytes.NewReader(body),
	)
	if err != nil {
		return nil, fmt.Errorf("creating request: %w", err)
	}
	req.Header.Set("Content-Type", "application/json")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("calling Telegram %s: %w", method, err)
	}
	defer resp.Body.Close() //nolint:errcheck

	respBody, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("reading response: %w", err)
	}

	var tgResp telegramResponse
	if err := json.Unmarshal(respBody, &tgResp); err != nil {
		return nil, fmt.Errorf("parsing response: %w", err)
	}

	if !tgResp.OK {
		return nil, fmt.Errorf("telegram API error: %s", tgResp.Description)
	}

	return &tgResp, nil
}

// textResult is a helper that wraps a string in an MCP CallToolResult.
func textResult(text string) (*mcp.CallToolResult, any, error) {
	return &mcp.CallToolResult{
		Content: []mcp.Content{
			&mcp.TextContent{Text: text},
		},
	}, nil, nil
}

// registerMessagingTools adds all Telegram messaging MCP tools to the server.
// Only tools whose names are in the allowed set are registered.
// If allowed is empty, all tools are registered.
func registerMessagingTools(server *mcp.Server, botToken string, allowed map[string]bool) {
	registerSendingTools(server, botToken, allowed)
	registerReadingTools(server, botToken, allowed)
	registerManagementTools(server, botToken, allowed)
}

func registerSendingTools(server *mcp.Server, botToken string, allowed map[string]bool) {
	if len(allowed) == 0 || allowed["send_message"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "send_message",
			Description: "Sends a text message to a Telegram chat.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *sendMessageParams) (*mcp.CallToolResult, any, error) {
			return handleSendMessage(ctx, botToken, params)
		})
	}

	if len(allowed) == 0 || allowed["send_photo"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "send_photo",
			Description: "Sends a photo to a Telegram chat by URL.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *sendPhotoParams) (*mcp.CallToolResult, any, error) {
			return handleSendPhoto(ctx, botToken, params)
		})
	}

	if len(allowed) == 0 || allowed["send_location"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "send_location",
			Description: "Sends a geographic location to a chat.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *sendLocationParams) (*mcp.CallToolResult, any, error) {
			return handleSendLocation(ctx, botToken, params)
		})
	}

	if len(allowed) == 0 || allowed["create_poll"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "create_poll",
			Description: "Creates a poll in a Telegram chat.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *createPollParams) (*mcp.CallToolResult, any, error) {
			return handleCreatePoll(ctx, botToken, params)
		})
	}
}

func registerReadingTools(server *mcp.Server, botToken string, allowed map[string]bool) {
	if len(allowed) == 0 || allowed["read_messages"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "read_messages",
			Description: "Reads recent messages (updates) received by the bot via getUpdates.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *readMessagesParams) (*mcp.CallToolResult, any, error) {
			return handleReadMessages(ctx, botToken, params)
		})
	}

	if len(allowed) == 0 || allowed["get_chat_info"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "get_chat_info",
			Description: "Gets detailed information about a chat.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *getChatInfoParams) (*mcp.CallToolResult, any, error) {
			return handleGetChatInfo(ctx, botToken, params)
		})
	}

	if len(allowed) == 0 || allowed["list_chats"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "list_chats",
			Description: "Lists unique chats the bot has received messages from.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *listChatsParams) (*mcp.CallToolResult, any, error) {
			return handleListChats(ctx, botToken, params)
		})
	}

	if len(allowed) == 0 || allowed["get_chat_members"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "get_chat_members",
			Description: "Gets the number of members in a chat.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *getChatMembersParams) (*mcp.CallToolResult, any, error) {
			return handleGetChatMembers(ctx, botToken, params)
		})
	}
}

func registerManagementTools(server *mcp.Server, botToken string, allowed map[string]bool) {
	if len(allowed) == 0 || allowed["forward_message"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "forward_message",
			Description: "Forwards a message from one chat to another.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *forwardMessageParams) (*mcp.CallToolResult, any, error) {
			return handleForwardMessage(ctx, botToken, params)
		})
	}

	if len(allowed) == 0 || allowed["edit_message"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "edit_message",
			Description: "Edits the text of a previously sent message.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *editMessageParams) (*mcp.CallToolResult, any, error) {
			return handleEditMessage(ctx, botToken, params)
		})
	}

	if len(allowed) == 0 || allowed["delete_message"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "delete_message",
			Description: "Deletes a message from a chat.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *deleteMessageParams) (*mcp.CallToolResult, any, error) {
			return handleDeleteMessage(ctx, botToken, params)
		})
	}

	if len(allowed) == 0 || allowed["pin_message"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "pin_message",
			Description: "Pins a message in a chat.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *pinMessageParams) (*mcp.CallToolResult, any, error) {
			return handlePinMessage(ctx, botToken, params)
		})
	}
}

// ── Parameter types and handlers ─────────────────────────────────────────────

// send_message

type sendMessageParams struct {
	ChatID    string `json:"chat_id" jsonschema:"required,Target chat ID or username"`
	Text      string `json:"text" jsonschema:"required,Text of the message to send"`
	ParseMode string `json:"parse_mode" jsonschema:"Optional parse mode: Markdown or HTML"`
}

func handleSendMessage(
	ctx context.Context, token string, params *sendMessageParams,
) (*mcp.CallToolResult, any, error) {
	payload := map[string]any{
		"chat_id": params.ChatID,
		"text":    params.Text,
	}
	if params.ParseMode != "" {
		payload["parse_mode"] = params.ParseMode
	}

	tgResp, err := callTelegram(ctx, token, "sendMessage", payload)
	if err != nil {
		return nil, nil, err
	}

	return textResult(fmt.Sprintf("Message sent successfully. Response: %s", string(tgResp.Result)))
}

// read_messages

type readMessagesParams struct {
	Offset  int `json:"offset" jsonschema:"Identifier of the first update to be returned"`
	Limit   int `json:"limit" jsonschema:"Max number of updates to retrieve (1-100)"`
	Timeout int `json:"timeout" jsonschema:"Timeout in seconds for long polling"`
}

func handleReadMessages(
	ctx context.Context, token string, params *readMessagesParams,
) (*mcp.CallToolResult, any, error) {
	payload := map[string]any{}
	if params.Offset != 0 {
		payload["offset"] = params.Offset
	}
	limit := params.Limit
	if limit <= 0 || limit > 100 {
		limit = 100
	}
	payload["limit"] = limit
	if params.Timeout > 0 {
		payload["timeout"] = params.Timeout
	}

	tgResp, err := callTelegram(ctx, token, "getUpdates", payload)
	if err != nil {
		return nil, nil, err
	}

	return textResult(fmt.Sprintf("Updates received: %s", string(tgResp.Result)))
}

// get_chat_info

type getChatInfoParams struct {
	ChatID string `json:"chat_id" jsonschema:"required,Target chat ID or username"`
}

func handleGetChatInfo(
	ctx context.Context, token string, params *getChatInfoParams,
) (*mcp.CallToolResult, any, error) {
	payload := map[string]any{
		"chat_id": params.ChatID,
	}

	tgResp, err := callTelegram(ctx, token, "getChat", payload)
	if err != nil {
		return nil, nil, err
	}

	return textResult(fmt.Sprintf("Chat info: %s", string(tgResp.Result)))
}

// send_photo

type sendPhotoParams struct {
	ChatID  string `json:"chat_id" jsonschema:"required,Target chat ID or username"`
	Photo   string `json:"photo" jsonschema:"required,Photo URL to send"`
	Caption string `json:"caption" jsonschema:"Optional caption for the photo"`
}

func handleSendPhoto(
	ctx context.Context, token string, params *sendPhotoParams,
) (*mcp.CallToolResult, any, error) {
	payload := map[string]any{
		"chat_id": params.ChatID,
		"photo":   params.Photo,
	}
	if params.Caption != "" {
		payload["caption"] = params.Caption
	}

	tgResp, err := callTelegram(ctx, token, "sendPhoto", payload)
	if err != nil {
		return nil, nil, err
	}

	return textResult(fmt.Sprintf("Photo sent successfully. Response: %s", string(tgResp.Result)))
}

// list_chats

type listChatsParams struct {
	Limit int `json:"limit" jsonschema:"Max updates to scan for chats (default 100)"`
}

func handleListChats(
	ctx context.Context, token string, params *listChatsParams,
) (*mcp.CallToolResult, any, error) {
	limit := params.Limit
	if limit <= 0 || limit > 100 {
		limit = 100
	}

	payload := map[string]any{
		"limit": limit,
	}

	tgResp, err := callTelegram(ctx, token, "getUpdates", payload)
	if err != nil {
		return nil, nil, err
	}

	// Parse updates to extract unique chats.
	var updates []struct {
		Message *struct {
			Chat struct {
				ID    int64  `json:"id"`
				Type  string `json:"type"`
				Title string `json:"title"`
				First string `json:"first_name"`
				Last  string `json:"last_name"`
				User  string `json:"username"`
			} `json:"chat"`
		} `json:"message"`
	}
	if err := json.Unmarshal(tgResp.Result, &updates); err != nil {
		return textResult(fmt.Sprintf("Raw updates: %s", string(tgResp.Result)))
	}

	seen := make(map[int64]bool)
	chatLines := make([]string, 0, len(updates))
	for _, u := range updates {
		if u.Message == nil {
			continue
		}
		c := u.Message.Chat
		if seen[c.ID] {
			continue
		}
		seen[c.ID] = true

		name := c.Title
		if name == "" {
			name = strings.TrimSpace(c.First + " " + c.Last)
		}
		if name == "" {
			name = c.User
		}
		chatLines = append(chatLines, fmt.Sprintf("- ID: %d, Type: %s, Name: %s", c.ID, c.Type, name))
	}

	if len(chatLines) == 0 {
		return textResult("No chats found in recent updates.")
	}

	return textResult(fmt.Sprintf("Found %d unique chat(s):\n%s", len(chatLines), strings.Join(chatLines, "\n")))
}

// forward_message

type forwardMessageParams struct {
	ChatID     string `json:"chat_id" jsonschema:"required,Target chat to forward the message to"`
	FromChatID string `json:"from_chat_id" jsonschema:"required,Source chat ID"`
	MessageID  int    `json:"message_id" jsonschema:"required,Message identifier in from_chat_id"`
}

func handleForwardMessage(
	ctx context.Context, token string, params *forwardMessageParams,
) (*mcp.CallToolResult, any, error) {
	payload := map[string]any{
		"chat_id":      params.ChatID,
		"from_chat_id": params.FromChatID,
		"message_id":   params.MessageID,
	}

	tgResp, err := callTelegram(ctx, token, "forwardMessage", payload)
	if err != nil {
		return nil, nil, err
	}

	return textResult(fmt.Sprintf("Message forwarded successfully. Response: %s", string(tgResp.Result)))
}

// edit_message

type editMessageParams struct {
	ChatID    string `json:"chat_id" jsonschema:"required,The chat containing the message to edit"`
	MessageID int    `json:"message_id" jsonschema:"required,Identifier of the message to edit"`
	Text      string `json:"text" jsonschema:"required,New text of the message"`
	ParseMode string `json:"parse_mode" jsonschema:"Optional parse mode: Markdown or HTML"`
}

func handleEditMessage(
	ctx context.Context, token string, params *editMessageParams,
) (*mcp.CallToolResult, any, error) {
	payload := map[string]any{
		"chat_id":    params.ChatID,
		"message_id": params.MessageID,
		"text":       params.Text,
	}
	if params.ParseMode != "" {
		payload["parse_mode"] = params.ParseMode
	}

	tgResp, err := callTelegram(ctx, token, "editMessageText", payload)
	if err != nil {
		return nil, nil, err
	}

	return textResult(fmt.Sprintf("Message edited successfully. Response: %s", string(tgResp.Result)))
}

// delete_message

type deleteMessageParams struct {
	ChatID    string `json:"chat_id" jsonschema:"required,The chat containing the message to delete"`
	MessageID int    `json:"message_id" jsonschema:"required,Identifier of the message to delete"`
}

func handleDeleteMessage(
	ctx context.Context, token string, params *deleteMessageParams,
) (*mcp.CallToolResult, any, error) {
	payload := map[string]any{
		"chat_id":    params.ChatID,
		"message_id": params.MessageID,
	}

	_, err := callTelegram(ctx, token, "deleteMessage", payload)
	if err != nil {
		return nil, nil, err
	}

	return textResult("Message deleted successfully.")
}

// pin_message

type pinMessageParams struct {
	ChatID              string `json:"chat_id" jsonschema:"required,The chat containing the message to pin"`
	MessageID           int    `json:"message_id" jsonschema:"required,Identifier of the message to pin"`
	DisableNotification bool   `json:"disable_notification" jsonschema:"Pin silently without notifying members"`
}

func handlePinMessage(
	ctx context.Context, token string, params *pinMessageParams,
) (*mcp.CallToolResult, any, error) {
	payload := map[string]any{
		"chat_id":    params.ChatID,
		"message_id": params.MessageID,
	}
	if params.DisableNotification {
		payload["disable_notification"] = true
	}

	_, err := callTelegram(ctx, token, "pinChatMessage", payload)
	if err != nil {
		return nil, nil, err
	}

	return textResult("Message pinned successfully.")
}

// get_chat_members

type getChatMembersParams struct {
	ChatID string `json:"chat_id" jsonschema:"required,Unique identifier for the target chat"`
}

func handleGetChatMembers(
	ctx context.Context, token string, params *getChatMembersParams,
) (*mcp.CallToolResult, any, error) {
	payload := map[string]any{
		"chat_id": params.ChatID,
	}

	tgResp, err := callTelegram(ctx, token, "getChatMembersCount", payload)
	if err != nil {
		return nil, nil, err
	}

	return textResult(fmt.Sprintf("Chat member count: %s", string(tgResp.Result)))
}

// send_location

type sendLocationParams struct {
	ChatID    string  `json:"chat_id" jsonschema:"required,Target chat ID or username"`
	Latitude  float64 `json:"latitude" jsonschema:"required,Latitude of the location"`
	Longitude float64 `json:"longitude" jsonschema:"required,Longitude of the location"`
}

func handleSendLocation(
	ctx context.Context, token string, params *sendLocationParams,
) (*mcp.CallToolResult, any, error) {
	payload := map[string]any{
		"chat_id":   params.ChatID,
		"latitude":  params.Latitude,
		"longitude": params.Longitude,
	}

	tgResp, err := callTelegram(ctx, token, "sendLocation", payload)
	if err != nil {
		return nil, nil, err
	}

	return textResult(fmt.Sprintf("Location sent successfully. Response: %s", string(tgResp.Result)))
}

// create_poll

type createPollParams struct {
	ChatID      string   `json:"chat_id" jsonschema:"required,Target chat ID or username"`
	Question    string   `json:"question" jsonschema:"required,Poll question (1-300 characters)"`
	Options     []string `json:"options" jsonschema:"required,Answer options (2-10 strings)"`
	IsAnonymous bool     `json:"is_anonymous" jsonschema:"True if poll should be anonymous"`
	Type        string   `json:"type" jsonschema:"Poll type: regular or quiz (default regular)"`
}

func handleCreatePoll(
	ctx context.Context, token string, params *createPollParams,
) (*mcp.CallToolResult, any, error) {
	payload := map[string]any{
		"chat_id":  params.ChatID,
		"question": params.Question,
		"options":  params.Options,
	}
	if params.IsAnonymous {
		payload["is_anonymous"] = true
	}
	if params.Type != "" {
		payload["type"] = params.Type
	}

	tgResp, err := callTelegram(ctx, token, "sendPoll", payload)
	if err != nil {
		return nil, nil, err
	}

	return textResult(fmt.Sprintf("Poll created successfully. Response: %s", string(tgResp.Result)))
}
