package slack

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
)

// callSlack makes a request to the Slack Web API and returns the raw response body.
func callSlack(ctx context.Context, token, method string, params url.Values) ([]byte, error) {
	reqURL := slackAPIBase + "/" + method

	req, err := http.NewRequestWithContext(
		ctx, http.MethodPost, reqURL, strings.NewReader(params.Encode()),
	)
	if err != nil {
		return nil, fmt.Errorf("creating request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")

	resp, err := slackHTTPClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("calling Slack %s: request failed", method)
	}
	defer resp.Body.Close() //nolint:errcheck

	body, err := io.ReadAll(io.LimitReader(resp.Body, 10*1024*1024))
	if err != nil {
		return nil, fmt.Errorf("reading response: %w", err)
	}

	var envelope struct {
		OK    bool   `json:"ok"`
		Error string `json:"error,omitempty"`
	}
	if err := json.Unmarshal(body, &envelope); err != nil {
		return nil, fmt.Errorf("parsing response: %w", err)
	}
	if !envelope.OK {
		return nil, fmt.Errorf("slack API error (%s): %s", method, envelope.Error)
	}

	return body, nil
}

// callSlackJSON makes a JSON-body request to the Slack Web API.
func callSlackJSON(ctx context.Context, token, method string, payload any) ([]byte, error) {
	reqURL := slackAPIBase + "/" + method

	jsonBody, err := json.Marshal(payload)
	if err != nil {
		return nil, fmt.Errorf("marshaling request: %w", err)
	}

	req, err := http.NewRequestWithContext(
		ctx, http.MethodPost, reqURL, strings.NewReader(string(jsonBody)),
	)
	if err != nil {
		return nil, fmt.Errorf("creating request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+token)
	req.Header.Set("Content-Type", "application/json; charset=utf-8")

	resp, err := slackHTTPClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("calling Slack %s: request failed", method)
	}
	defer resp.Body.Close() //nolint:errcheck

	body, err := io.ReadAll(io.LimitReader(resp.Body, 10*1024*1024))
	if err != nil {
		return nil, fmt.Errorf("reading response: %w", err)
	}

	var envelope struct {
		OK    bool   `json:"ok"`
		Error string `json:"error,omitempty"`
	}
	if err := json.Unmarshal(body, &envelope); err != nil {
		return nil, fmt.Errorf("parsing response: %w", err)
	}
	if !envelope.OK {
		return nil, fmt.Errorf("slack API error (%s): %s", method, envelope.Error)
	}

	return body, nil
}

// textResult is a helper that wraps a string in an MCP CallToolResult.
func textResult(text string) (*mcp.CallToolResult, any, error) {
	return &mcp.CallToolResult{
		Content: []mcp.Content{
			&mcp.TextContent{Text: text},
		},
	}, nil, nil
}

// registerMessagingTools adds all Slack messaging MCP tools to the server.
// Only tools whose names are in the allowed set are registered.
// If allowed is empty, all tools are registered.
func registerMessagingTools(server *mcp.Server, token string, allowed map[string]bool) {
	registerChannelTools(server, token, allowed)
	registerMessageTools(server, token, allowed)
	registerWorkspaceTools(server, token, allowed)
}

func registerChannelTools(server *mcp.Server, token string, allowed map[string]bool) {
	if len(allowed) == 0 || allowed["list_channels"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "list_channels",
			Description: "Lists Slack channels (public and private) the bot has access to.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *listChannelsParams) (*mcp.CallToolResult, any, error) {
			return handleListChannels(ctx, token, params)
		})
	}

	if len(allowed) == 0 || allowed["get_channel_info"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "get_channel_info",
			Description: "Gets detailed information about a Slack channel.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *getChannelInfoParams) (*mcp.CallToolResult, any, error) {
			return handleGetChannelInfo(ctx, token, params)
		})
	}

	if len(allowed) == 0 || allowed["read_messages"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "read_messages",
			Description: "Reads recent messages from a Slack channel.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *readMessagesParams) (*mcp.CallToolResult, any, error) {
			return handleReadMessages(ctx, token, params)
		})
	}
}

func registerMessageTools(server *mcp.Server, token string, allowed map[string]bool) {
	if len(allowed) == 0 || allowed["send_message"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "send_message",
			Description: "Sends a message to a Slack channel.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *sendMessageParams) (*mcp.CallToolResult, any, error) {
			return handleSendMessage(ctx, token, params)
		})
	}

	if len(allowed) == 0 || allowed["send_reply"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "send_reply",
			Description: "Sends a threaded reply to a message in a Slack channel.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *sendReplyParams) (*mcp.CallToolResult, any, error) {
			return handleSendReply(ctx, token, params)
		})
	}
}

func registerWorkspaceTools(server *mcp.Server, token string, allowed map[string]bool) {
	if len(allowed) == 0 || allowed["list_users"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "list_users",
			Description: "Lists users in the Slack workspace.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *listUsersParams) (*mcp.CallToolResult, any, error) {
			return handleListUsers(ctx, token, params)
		})
	}

	if len(allowed) == 0 || allowed["search_messages"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "search_messages",
			Description: "Searches messages across the Slack workspace.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *searchMessagesParams) (*mcp.CallToolResult, any, error) {
			return handleSearchMessages(ctx, token, params)
		})
	}
}

// ── Parameter types and handlers ─────────────────────────────────────────────

// list_channels

type listChannelsParams struct {
	Limit  int    `json:"limit" jsonschema:"Maximum number of channels to return (default 100, max 1000)"`
	Cursor string `json:"cursor" jsonschema:"Pagination cursor for next page of results"`
}

func handleListChannels(
	ctx context.Context, token string, params *listChannelsParams,
) (*mcp.CallToolResult, any, error) {
	v := url.Values{}
	v.Set("types", "public_channel,private_channel")
	limit := params.Limit
	if limit <= 0 || limit > 1000 {
		limit = 100
	}
	v.Set("limit", fmt.Sprintf("%d", limit))
	if params.Cursor != "" {
		v.Set("cursor", params.Cursor)
	}

	body, err := callSlack(ctx, token, "conversations.list", v)
	if err != nil {
		return nil, nil, err
	}

	return textResult(string(body))
}

// send_message

type sendMessageParams struct {
	Channel string `json:"channel" jsonschema:"required,Channel ID to send the message to"`
	Text    string `json:"text" jsonschema:"required,Text of the message to send"`
}

func handleSendMessage(
	ctx context.Context, token string, params *sendMessageParams,
) (*mcp.CallToolResult, any, error) {
	payload := map[string]any{
		"channel": params.Channel,
		"text":    params.Text,
	}

	body, err := callSlackJSON(ctx, token, "chat.postMessage", payload)
	if err != nil {
		return nil, nil, err
	}

	return textResult(fmt.Sprintf("Message sent successfully. Response: %s", string(body)))
}

// read_messages

type readMessagesParams struct {
	Channel string `json:"channel" jsonschema:"required,Channel ID to read messages from"`
	Limit   int    `json:"limit" jsonschema:"Maximum number of messages to return (default 20, max 100)"`
	Cursor  string `json:"cursor" jsonschema:"Pagination cursor for next page of results"`
}

func handleReadMessages(
	ctx context.Context, token string, params *readMessagesParams,
) (*mcp.CallToolResult, any, error) {
	v := url.Values{}
	v.Set("channel", params.Channel)
	limit := params.Limit
	if limit <= 0 || limit > 100 {
		limit = 20
	}
	v.Set("limit", fmt.Sprintf("%d", limit))
	if params.Cursor != "" {
		v.Set("cursor", params.Cursor)
	}

	body, err := callSlack(ctx, token, "conversations.history", v)
	if err != nil {
		return nil, nil, err
	}

	return textResult(string(body))
}

// send_reply

type sendReplyParams struct {
	Channel  string `json:"channel" jsonschema:"required,Channel ID containing the thread"`
	ThreadTS string `json:"thread_ts" jsonschema:"required,Timestamp of the parent message to reply to"`
	Text     string `json:"text" jsonschema:"required,Text of the reply"`
}

func handleSendReply(
	ctx context.Context, token string, params *sendReplyParams,
) (*mcp.CallToolResult, any, error) {
	payload := map[string]any{
		"channel":   params.Channel,
		"thread_ts": params.ThreadTS,
		"text":      params.Text,
	}

	body, err := callSlackJSON(ctx, token, "chat.postMessage", payload)
	if err != nil {
		return nil, nil, err
	}

	return textResult(fmt.Sprintf("Reply sent successfully. Response: %s", string(body)))
}

// get_channel_info

type getChannelInfoParams struct {
	Channel string `json:"channel" jsonschema:"required,Channel ID to get info for"`
}

func handleGetChannelInfo(
	ctx context.Context, token string, params *getChannelInfoParams,
) (*mcp.CallToolResult, any, error) {
	v := url.Values{}
	v.Set("channel", params.Channel)

	body, err := callSlack(ctx, token, "conversations.info", v)
	if err != nil {
		return nil, nil, err
	}

	return textResult(string(body))
}

// list_users

type listUsersParams struct {
	Limit  int    `json:"limit" jsonschema:"Maximum number of users to return (default 100, max 1000)"`
	Cursor string `json:"cursor" jsonschema:"Pagination cursor for next page of results"`
}

func handleListUsers(
	ctx context.Context, token string, params *listUsersParams,
) (*mcp.CallToolResult, any, error) {
	v := url.Values{}
	limit := params.Limit
	if limit <= 0 || limit > 1000 {
		limit = 100
	}
	v.Set("limit", fmt.Sprintf("%d", limit))
	if params.Cursor != "" {
		v.Set("cursor", params.Cursor)
	}

	body, err := callSlack(ctx, token, "users.list", v)
	if err != nil {
		return nil, nil, err
	}

	return textResult(string(body))
}

// search_messages

type searchMessagesParams struct {
	Query string `json:"query" jsonschema:"required,Search query string"`
	Count int    `json:"count" jsonschema:"Number of results to return per page (default 20, max 100)"`
	Page  int    `json:"page" jsonschema:"Page number of results to return (default 1)"`
}

func handleSearchMessages(
	ctx context.Context, token string, params *searchMessagesParams,
) (*mcp.CallToolResult, any, error) {
	v := url.Values{}
	v.Set("query", params.Query)
	count := params.Count
	if count <= 0 || count > 100 {
		count = 20
	}
	v.Set("count", fmt.Sprintf("%d", count))
	page := params.Page
	if page <= 0 {
		page = 1
	}
	v.Set("page", fmt.Sprintf("%d", page))

	body, err := callSlack(ctx, token, "search.messages", v)
	if err != nil {
		return nil, nil, err
	}

	return textResult(string(body))
}
