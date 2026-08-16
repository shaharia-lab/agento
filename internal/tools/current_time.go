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

// FormatCurrentTime renders the current_time tool's answer for tz at now.
//
// It is the whole body of the tool handler, with the clock passed in, and it is
// exported so desktop/parity can generate cross-language vectors from the real
// implementation rather than from a restatement of it — a vector taken against
// time.Now() would pin one instant on one machine. The returned error is the
// text the model reads: mcp.AddTool packs a handler error into
// CallToolResult.Content with IsError set.
func FormatCurrentTime(tz string, now time.Time) (string, error) {
	if tz == "" {
		tz = "UTC"
	}

	loc, err := time.LoadLocation(tz)
	if err != nil {
		return "", fmt.Errorf("unknown timezone %q: %w", tz, err)
	}

	local := now.In(loc)
	return fmt.Sprintf(
		"Current time in %s: %s (ISO 8601: %s)",
		tz,
		local.Format(time.RFC1123),
		local.Format(time.RFC3339),
	), nil
}

// getCurrentTime returns the current time in the requested timezone.
func getCurrentTime(
	_ context.Context, _ *mcp.CallToolRequest, params *currentTimeParams,
) (*mcp.CallToolResult, any, error) {
	text, err := FormatCurrentTime(params.Timezone, time.Now())
	if err != nil {
		return nil, nil, err
	}

	return &mcp.CallToolResult{
		Content: []mcp.Content{
			&mcp.TextContent{Text: text},
		},
	}, nil, nil
}
