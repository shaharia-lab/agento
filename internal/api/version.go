package api

import (
	"context"
	"fmt"
	"net/http"
	"strings"
	"sync"
	"time"

	"github.com/creativeprojects/go-selfupdate"

	"github.com/shaharia-lab/agento/internal/build"
)

type updateCheckResult struct {
	UpdateAvailable bool
	LatestVersion   string
	ReleaseURL      string
}

// updateCheckCache holds the cached result of the GitHub release check.
// It is stored on the Server struct so each server instance has its own cache,
// keeping tests isolated and state contained.
type updateCheckCache struct {
	mu        sync.Mutex
	result    *updateCheckResult
	fetchedAt time.Time
}

const updateCheckTTL = time.Hour

func (s *Server) handleVersion(w http.ResponseWriter, r *http.Request) {
	s.writeJSON(w, http.StatusOK, map[string]string{
		"version":    build.Version,
		"commit":     build.CommitSHA,
		"build_date": build.BuildDate,
	})
}

// handleUpdateCheck reports whether a newer release is available on GitHub.
// Results are cached for 1 hour per server instance.
func (s *Server) handleUpdateCheck(w http.ResponseWriter, r *http.Request) {
	current := build.Version
	currentTrimmed := strings.TrimPrefix(current, "v")

	// Dev/unknown builds cannot be compared to releases.
	if currentTrimmed == "dev" || currentTrimmed == "unknown" {
		s.writeJSON(w, http.StatusOK, map[string]interface{}{
			"current_version":  current,
			"update_available": false,
			"latest_version":   "",
			"release_url":      "",
		})
		return
	}

	result, err := s.cachedUpdateCheck(r.Context(), currentTrimmed)
	if err != nil {
		s.logger.Error("update check failed", "error", err)
		s.writeError(w, http.StatusBadGateway, "failed to check for updates")
		return
	}

	s.writeJSON(w, http.StatusOK, map[string]interface{}{
		"current_version":  current,
		"update_available": result.UpdateAvailable,
		"latest_version":   result.LatestVersion,
		"release_url":      result.ReleaseURL,
	})
}

// cachedUpdateCheck returns a cached result or fetches the latest release from GitHub.
// The cache TTL is 1 hour; it is keyed by time only since the running binary version
// does not change between requests.
func (s *Server) cachedUpdateCheck(ctx context.Context, currentTrimmed string) (*updateCheckResult, error) {
	s.updateCache.mu.Lock()
	defer s.updateCache.mu.Unlock()

	if s.updateCache.result != nil && time.Since(s.updateCache.fetchedAt) < updateCheckTTL {
		return s.updateCache.result, nil
	}

	updater, err := selfupdate.NewUpdater(selfupdate.Config{})
	if err != nil {
		return nil, fmt.Errorf("creating updater: %w", err)
	}

	release, found, err := updater.DetectLatest(ctx, selfupdate.ParseSlug("shaharia-lab/agento"))
	if err != nil {
		return nil, fmt.Errorf("detecting latest release: %w", err)
	}

	result := &updateCheckResult{}
	if found && release.GreaterThan(currentTrimmed) {
		result.UpdateAvailable = true
		result.LatestVersion = release.Version()
		result.ReleaseURL = fmt.Sprintf("https://github.com/shaharia-lab/agento/releases/tag/v%s", release.Version())
	}

	s.updateCache.result = result
	s.updateCache.fetchedAt = time.Now()
	return result, nil
}
