package whatsapp

import (
	"context"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"strings"
	"time"
)

// maxMediaSize is the maximum allowed media download size (15 MB).
// WhatsApp's own limits are 16 MB for video and lower for other types.
const maxMediaSize = 15 * 1024 * 1024

// safeDialContext dials network addresses but rejects connections to private or
// reserved IP addresses at connection time (not as a pre-check), preventing
// DNS rebinding / TOCTOU SSRF attacks.
func safeDialContext(ctx context.Context, network, addr string) (net.Conn, error) {
	host, port, err := net.SplitHostPort(addr)
	if err != nil {
		return nil, fmt.Errorf("parsing dial address: %w", err)
	}

	ips, err := net.DefaultResolver.LookupHost(ctx, host)
	if err != nil {
		return nil, fmt.Errorf("resolving host %q: %w", host, err)
	}

	for _, ipStr := range ips {
		if ip := net.ParseIP(ipStr); ip != nil && isPrivateOrReservedIP(ip) {
			return nil, fmt.Errorf("blocked connection to private/reserved IP %s", ipStr)
		}
	}

	return (&net.Dialer{}).DialContext(ctx, network, net.JoinHostPort(ips[0], port))
}

// httpClient is used for downloading media from URLs. The transport uses
// safeDialContext so IP validation happens at connection time, preventing
// DNS rebinding attacks. CheckRedirect limits redirects to 3 and rejects
// non-HTTP(S) redirect targets; safeDialContext handles IP validation at dial time.
var httpClient = &http.Client{ //nolint:gochecknoglobals
	Timeout:   60 * time.Second,
	Transport: &http.Transport{DialContext: safeDialContext},
	CheckRedirect: func(req *http.Request, via []*http.Request) error {
		if len(via) >= 3 {
			return fmt.Errorf("too many redirects")
		}
		if req.URL.Scheme != "http" && req.URL.Scheme != "https" {
			return fmt.Errorf("redirect to non-HTTP scheme %q", req.URL.Scheme)
		}
		return nil
	},
}

// validateMediaURL checks that the URL uses an http or https scheme and has a
// non-empty host. It does NOT perform DNS resolution — safeDialContext is the
// actual SSRF guard and resolves addresses at connection time, preventing TOCTOU
// attacks where the DNS response changes between pre-check and dial.
func validateMediaURL(mediaURL string) error {
	parsed, err := url.Parse(mediaURL)
	if err != nil {
		return fmt.Errorf("invalid URL: %w", err)
	}

	scheme := strings.ToLower(parsed.Scheme)
	if scheme != "http" && scheme != "https" {
		return fmt.Errorf("unsupported URL scheme %q: only http and https are allowed", parsed.Scheme)
	}

	host := parsed.Hostname()
	if host == "" {
		return fmt.Errorf("URL has no host")
	}

	return nil
}

// isPrivateOrReservedIP returns true if the IP is loopback, link-local,
// private (RFC 1918 / RFC 4193), or otherwise reserved.
func isPrivateOrReservedIP(ip net.IP) bool {
	return ip.IsLoopback() ||
		ip.IsPrivate() ||
		ip.IsLinkLocalUnicast() ||
		ip.IsLinkLocalMulticast() ||
		ip.IsUnspecified()
}

// downloadMedia downloads media content from a URL with a size limit.
// The URL is validated against SSRF before making the request.
func downloadMedia(ctx context.Context, mediaURL string) ([]byte, error) {
	if err := validateMediaURL(mediaURL); err != nil {
		return nil, fmt.Errorf("URL validation failed: %w", err)
	}

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

	// Reject immediately if Content-Length header reports oversized payload.
	if resp.ContentLength > maxMediaSize {
		return nil, fmt.Errorf("media too large: %d bytes (max %d)", resp.ContentLength, maxMediaSize)
	}

	// Read one byte over the limit to detect truncation vs exact-size files.
	data, err := io.ReadAll(io.LimitReader(resp.Body, maxMediaSize+1))
	if err != nil {
		return nil, fmt.Errorf("reading media: %w", err)
	}
	if int64(len(data)) > maxMediaSize {
		return nil, fmt.Errorf("media exceeds %d bytes", maxMediaSize)
	}

	return data, nil
}

// detectMIMEType detects the MIME type of data using http.DetectContentType.
func detectMIMEType(data []byte) string {
	return http.DetectContentType(data)
}
