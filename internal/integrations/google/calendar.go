package google

import (
	"context"
	"fmt"
	"net/http"
	"time"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
	"google.golang.org/api/calendar/v3"
	"google.golang.org/api/option"
)

// registerCalendarTools adds Google Calendar MCP tools to the server.
func registerCalendarTools(server *mcp.Server, httpClient *http.Client) {
	calSvc, err := calendar.NewService(context.Background(), option.WithHTTPClient(httpClient))
	if err != nil {
		// If we can't create the service, skip registration — server will start without calendar tools.
		return
	}

	mcp.AddTool(server, &mcp.Tool{
		Name:        "create_event",
		Description: "Creates a new event on the user's primary Google Calendar.",
	}, func(ctx context.Context, _ *mcp.CallToolRequest, params *createEventParams) (*mcp.CallToolResult, any, error) {
		return handleCreateEvent(ctx, calSvc, params)
	})

	mcp.AddTool(server, &mcp.Tool{
		Name:        "view_events",
		Description: "Lists events from the user's primary Google Calendar within an optional time range.",
	}, func(ctx context.Context, _ *mcp.CallToolRequest, params *viewEventsParams) (*mcp.CallToolResult, any, error) {
		return handleViewEvents(ctx, calSvc, params)
	})
}

type createEventParams struct {
	Summary     string `json:"summary" jsonschema:"required,The title of the event"`
	Start       string `json:"start" jsonschema:"required,Start time in RFC3339 format (e.g. 2026-03-01T10:00:00-07:00)"`
	End         string `json:"end" jsonschema:"required,End time in RFC3339 format"`
	Description string `json:"description" jsonschema:"Optional description of the event"`
}

func handleCreateEvent(ctx context.Context, svc *calendar.Service, params *createEventParams) (*mcp.CallToolResult, any, error) {
	event := &calendar.Event{
		Summary:     params.Summary,
		Description: params.Description,
		Start:       &calendar.EventDateTime{DateTime: params.Start, TimeZone: "UTC"},
		End:         &calendar.EventDateTime{DateTime: params.End, TimeZone: "UTC"},
	}

	created, err := svc.Events.Insert("primary", event).Context(ctx).Do()
	if err != nil {
		return nil, nil, fmt.Errorf("creating calendar event: %w", err)
	}

	return &mcp.CallToolResult{
		Content: []mcp.Content{
			&mcp.TextContent{
				Text: fmt.Sprintf("Event created: %s\nID: %s\nLink: %s", created.Summary, created.Id, created.HtmlLink),
			},
		},
	}, nil, nil
}

type viewEventsParams struct {
	TimeMin    string `json:"time_min" jsonschema:"Lower bound for event end time in RFC3339 format. Defaults to now."`
	TimeMax    string `json:"time_max" jsonschema:"Upper bound for event start time in RFC3339 format."`
	MaxResults int64  `json:"max_results" jsonschema:"Maximum number of events to return (default 10, max 100)"`
}

func handleViewEvents(ctx context.Context, svc *calendar.Service, params *viewEventsParams) (*mcp.CallToolResult, any, error) {
	maxResults := params.MaxResults
	if maxResults <= 0 || maxResults > 100 {
		maxResults = 10
	}

	timeMin := params.TimeMin
	if timeMin == "" {
		timeMin = time.Now().UTC().Format(time.RFC3339)
	}

	call := svc.Events.List("primary").
		Context(ctx).
		MaxResults(maxResults).
		SingleEvents(true).
		OrderBy("startTime").
		TimeMin(timeMin)

	if params.TimeMax != "" {
		call = call.TimeMax(params.TimeMax)
	}

	events, err := call.Do()
	if err != nil {
		return nil, nil, fmt.Errorf("listing calendar events: %w", err)
	}

	if len(events.Items) == 0 {
		return &mcp.CallToolResult{
			Content: []mcp.Content{&mcp.TextContent{Text: "No events found in the specified range."}},
		}, nil, nil
	}

	result := fmt.Sprintf("Found %d event(s):\n", len(events.Items))
	for _, ev := range events.Items {
		start := ev.Start.DateTime
		if start == "" {
			start = ev.Start.Date
		}
		result += fmt.Sprintf("\n- %s\n  Start: %s\n  ID: %s\n  Link: %s\n",
			ev.Summary, start, ev.Id, ev.HtmlLink)
	}

	return &mcp.CallToolResult{
		Content: []mcp.Content{&mcp.TextContent{Text: result}},
	}, nil, nil
}
