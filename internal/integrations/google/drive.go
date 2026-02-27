package google

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"strings"

	mcp "github.com/modelcontextprotocol/go-sdk/mcp"
	"google.golang.org/api/drive/v3"
	"google.golang.org/api/option"
)

// registerDriveTools adds Google Drive MCP tools to the server.
// Only tools whose names are in the allowed set are registered.
// If allowed is empty, all tools are registered.
func registerDriveTools(server *mcp.Server, httpClient *http.Client, allowed map[string]bool) {
	driveSvc, err := drive.NewService(context.Background(), option.WithHTTPClient(httpClient))
	if err != nil {
		return
	}

	if len(allowed) == 0 || allowed["list_files"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "list_files",
			Description: "Lists files and folders in Google Drive.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *listFilesParams) (*mcp.CallToolResult, any, error) {
			return handleListFiles(ctx, driveSvc, params)
		})
	}

	if len(allowed) == 0 || allowed["create_file"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "create_file",
			Description: "Creates a new file in Google Drive with the provided content.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *createFileParams) (*mcp.CallToolResult, any, error) {
			return handleCreateFile(ctx, driveSvc, params)
		})
	}

	if len(allowed) == 0 || allowed["download_file"] {
		mcp.AddTool(server, &mcp.Tool{
			Name:        "download_file",
			Description: "Downloads and returns the text content of a Google Drive file by its ID.",
		}, func(ctx context.Context, _ *mcp.CallToolRequest, params *downloadFileParams) (*mcp.CallToolResult, any, error) {
			return handleDownloadFile(ctx, driveSvc, params)
		})
	}
}

type listFilesParams struct {
	Query      string `json:"query" jsonschema:"Optional Drive query string (e.g. \"name contains 'report'\")"`
	MaxResults int64  `json:"max_results" jsonschema:"Maximum number of files to return (default 10, max 100)"`
}

func handleListFiles(ctx context.Context, svc *drive.Service, params *listFilesParams) (*mcp.CallToolResult, any, error) {
	maxResults := params.MaxResults
	if maxResults <= 0 || maxResults > 100 {
		maxResults = 10
	}

	call := svc.Files.List().
		Context(ctx).
		PageSize(maxResults).
		Fields("files(id,name,mimeType,size,modifiedTime,webViewLink)")

	if params.Query != "" {
		call = call.Q(params.Query)
	}

	list, err := call.Do()
	if err != nil {
		return nil, nil, fmt.Errorf("listing drive files: %w", err)
	}

	if len(list.Files) == 0 {
		return &mcp.CallToolResult{
			Content: []mcp.Content{&mcp.TextContent{Text: "No files found."}},
		}, nil, nil
	}

	rows := make([]string, 0, len(list.Files))
	for _, f := range list.Files {
		rows = append(rows, fmt.Sprintf("Name: %s\nID: %s\nType: %s\nModified: %s\nLink: %s",
			f.Name, f.Id, f.MimeType, f.ModifiedTime, f.WebViewLink))
	}

	return &mcp.CallToolResult{
		Content: []mcp.Content{&mcp.TextContent{
			Text: fmt.Sprintf("Found %d file(s):\n\n%s", len(list.Files), strings.Join(rows, "\n\n---\n\n")),
		}},
	}, nil, nil
}

type createFileParams struct {
	Name     string `json:"name" jsonschema:"required,Name of the file to create"`
	Content  string `json:"content" jsonschema:"required,Text content of the file"`
	MimeType string `json:"mime_type" jsonschema:"MIME type (default: text/plain)"`
}

func handleCreateFile(ctx context.Context, svc *drive.Service, params *createFileParams) (*mcp.CallToolResult, any, error) {
	mimeType := params.MimeType
	if mimeType == "" {
		mimeType = "text/plain"
	}

	meta := &drive.File{
		Name:     params.Name,
		MimeType: mimeType,
	}

	created, err := svc.Files.Create(meta).
		Context(ctx).
		Media(strings.NewReader(params.Content)).
		Fields("id,name,webViewLink").
		Do()
	if err != nil {
		return nil, nil, fmt.Errorf("creating drive file: %w", err)
	}

	return &mcp.CallToolResult{
		Content: []mcp.Content{
			&mcp.TextContent{
				Text: fmt.Sprintf("File created: %s\nID: %s\nLink: %s", created.Name, created.Id, created.WebViewLink),
			},
		},
	}, nil, nil
}

type downloadFileParams struct {
	FileID string `json:"file_id" jsonschema:"required,The Google Drive file ID to download"`
}

func handleDownloadFile(ctx context.Context, svc *drive.Service, params *downloadFileParams) (*mcp.CallToolResult, any, error) {
	resp, err := svc.Files.Get(params.FileID).Context(ctx).Download()
	if err != nil {
		return nil, nil, fmt.Errorf("downloading drive file %q: %w", params.FileID, err)
	}
	defer resp.Body.Close() //nolint:errcheck

	data, readErr := io.ReadAll(resp.Body)
	if readErr != nil {
		return nil, nil, fmt.Errorf("reading file content: %w", readErr)
	}

	return &mcp.CallToolResult{
		Content: []mcp.Content{&mcp.TextContent{Text: string(data)}},
	}, nil, nil
}
