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
// DNS rebinding attacks.
var httpClient = &http.Client{ //nolint:gochecknoglobals
	Timeout:   60 * time.Second,
	Transport: &http.Transport{DialContext: safeDialContext},
}

// validateMediaURL checks that the URL is safe to fetch (no SSRF).
// It rejects non-HTTP(S) schemes, loopback addresses, and private/internal IP ranges.
func validateMediaURL(mediaURL string) error {
	return validateMediaURLWithResolver(mediaURL, net.DefaultResolver)
}

// validateMediaURLWithResolver is the internal implementation that accepts a resolver for testing.
func validateMediaURLWithResolver(mediaURL string, resolver *net.Resolver) error {
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

	// Resolve the hostname to IP addresses and check each one.
	ips, err := resolver.LookupHost(context.Background(), host)
	if err != nil {
		return fmt.Errorf("resolving host %q: %w", host, err)
	}

	for _, ipStr := range ips {
		ip := net.ParseIP(ipStr)
		if ip == nil {
			return fmt.Errorf("invalid resolved IP %q for host %q", ipStr, host)
		}
		if isPrivateOrReservedIP(ip) {
			return fmt.Errorf("URL resolves to private/reserved IP %s", ipStr)
		}
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
