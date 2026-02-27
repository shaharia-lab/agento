package tools

import (
	"context"
	"fmt"
	"time"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
)

// currentTimeParams is the input schema for the current_time tool.
type currentTimeParams struct {
	Timezone string `json:"timezone" jsonschema:"IANA timezone name, e.g. UTC or America/New_York. Defaults to UTC."`
}

// getCurrentTime returns the current time in the requested timezone.
func getCurrentTime(_ context.Context, _ *mcp.CallToolRequest, params *currentTimeParams) (*mcp.CallToolResult, any, error) {
	tz := params.Timezone
	if tz == "" {
		tz = "UTC"
	}

	loc, err := time.LoadLocation(tz)
	if err != nil {
		return nil, nil, fmt.Errorf("unknown timezone %q: %w", tz, err)
	}

	now := time.Now().In(loc)
	return &mcp.CallToolResult{
		Content: []mcp.Content{
			&mcp.TextContent{
				Text: fmt.Sprintf(
					"Current time in %s: %s (ISO 8601: %s)",
					tz,
					now.Format(time.RFC1123),
					now.Format(time.RFC3339),
				),
			},
		},
	}, nil, nil
}
