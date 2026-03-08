package whatsapp

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"time"
)

// httpClient is used for downloading media from URLs.
var httpClient = &http.Client{Timeout: 60 * time.Second} //nolint:gochecknoglobals

// maxMediaSize is the maximum allowed media download size (50 MB).
const maxMediaSize = 50 * 1024 * 1024

// downloadMedia downloads media content from a URL with a size limit.
func downloadMedia(ctx context.Context, mediaURL string) ([]byte, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, mediaURL, nil)
	if err != nil {
		return nil, fmt.Errorf("creating request: %w", err)
	}

	resp, err := httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("downloading media: %w", err)
	}
	defer resp.Body.Close() //nolint:errcheck

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("download returned status %d", resp.StatusCode)
	}

	data, err := io.ReadAll(io.LimitReader(resp.Body, maxMediaSize))
	if err != nil {
		return nil, fmt.Errorf("reading media: %w", err)
	}

	return data, nil
}

// detectMIMEType detects the MIME type of data using http.DetectContentType.
func detectMIMEType(data []byte) string {
	return http.DetectContentType(data)
}
