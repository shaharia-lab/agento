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

// updateCheckCache caches the GitHub release check result to avoid hammering the API.
var updateCheckCache struct {
	mu        sync.Mutex
	result    *updateCheckResult
	fetchedAt time.Time
}

type updateCheckResult struct {
	UpdateAvailable bool
	LatestVersion   string
	ReleaseURL      string
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
// Results are cached for 1 hour.
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

	result, err := s.cachedUpdateCheck(currentTrimmed)
	if err != nil {
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
func (s *Server) cachedUpdateCheck(currentTrimmed string) (*updateCheckResult, error) {
	updateCheckCache.mu.Lock()
	defer updateCheckCache.mu.Unlock()

	if updateCheckCache.result != nil && time.Since(updateCheckCache.fetchedAt) < updateCheckTTL {
		return updateCheckCache.result, nil
	}

	updater, err := selfupdate.NewUpdater(selfupdate.Config{})
	if err != nil {
		return nil, fmt.Errorf("creating updater: %w", err)
	}

	release, found, err := updater.DetectLatest(context.Background(), selfupdate.ParseSlug("shaharia-lab/agento"))
	if err != nil {
		return nil, fmt.Errorf("detecting latest release: %w", err)
	}

	result := &updateCheckResult{}
	if found && release.GreaterThan(currentTrimmed) {
		result.UpdateAvailable = true
		result.LatestVersion = release.Version()
		result.ReleaseURL = fmt.Sprintf("https://github.com/shaharia-lab/agento/releases/tag/v%s", release.Version())
	}

	updateCheckCache.result = result
	updateCheckCache.fetchedAt = time.Now()
	return result, nil
}
