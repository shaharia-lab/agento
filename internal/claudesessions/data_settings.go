package claudesessions

import (
	"sort"
	"sync"
	"time"

	"github.com/shaharia-lab/agento/internal/config"
)

// dataSettings holds the process-wide snapshot of the user's Data & Analytics
// preferences: what counts as continuous work, and which projects are excluded
// from every figure Agento reports.
//
// A process-wide snapshot rather than a parameter threaded through the call
// graph, for the same reason packagePricing is one: the readers are the
// scanner, all nine insight processors, the journey builder and the session
// list, none of which have a settings dependency and all of which must agree.
// It is written by ApplyDataSettings during startup wiring and again whenever
// the settings are saved; the mutex serializes those writes against reads.
var dataSettings = struct {
	sync.RWMutex
	idleGap time.Duration
	hidden  map[string]struct{}
}{idleGap: config.DefaultIdleGapThresholdMinutes * time.Minute}

// ApplyDataSettings installs the user's Data & Analytics preferences.
//
// An idleGapMinutes outside the configured bounds — including the zero value a
// settings row carries before the user has ever chosen — falls back to the
// default rather than erroring: an unset preference is not a misconfiguration,
// and SettingsManager.Update rejects real input that is out of range before it
// ever reaches here.
//
// hiddenProjects are decoded project paths, matched against
// ClaudeSessionSummary.ProjectPath exactly.
func ApplyDataSettings(idleGapMinutes int, hiddenProjects []string) {
	if idleGapMinutes < config.MinIdleGapThresholdMinutes ||
		idleGapMinutes > config.MaxIdleGapThresholdMinutes {
		idleGapMinutes = config.DefaultIdleGapThresholdMinutes
	}
	hidden := make(map[string]struct{}, len(hiddenProjects))
	for _, p := range hiddenProjects {
		if p != "" {
			hidden[p] = struct{}{}
		}
	}

	dataSettings.Lock()
	defer dataSettings.Unlock()
	dataSettings.idleGap = time.Duration(idleGapMinutes) * time.Minute
	dataSettings.hidden = hidden
}

// IdleGapThreshold is the largest gap between two consecutive transcript
// events that still counts as continuous work. Claude Code sessions are
// resumable: a transcript's wall-clock span routinely contains lunch breaks,
// nights, or a resume weeks later, and on the reference corpus a single
// resumed-after-28-days session carried 82% of the dashboard's "Avg Duration"
// on its own. Ten minutes — the default — keeps reading a long reply or
// manually testing a change inside a sitting while excluding everything a
// person would not call working time.
//
// Every consumer of "how long did this actually run" shares this one value —
// the scanner, the insight processors, and the journey builder — so no two
// pages can disagree about what active time means. It is user-configurable
// because the right figure depends on how someone works, and changing it
// re-reads every transcript and reprocesses every insight row: the durations
// derived from it are stored, not computed on read. See idleThresholdStaleness.
func IdleGapThreshold() time.Duration {
	dataSettings.RLock()
	defer dataSettings.RUnlock()
	return dataSettings.idleGap
}

// IsProjectHidden reports whether the user has excluded a project from
// reporting. Hidden means hidden, not deleted: the transcripts are still
// scanned and their rows still cached, so unhiding is immediate and costs no
// re-read.
func IsProjectHidden(projectPath string) bool {
	dataSettings.RLock()
	defer dataSettings.RUnlock()
	_, hidden := dataSettings.hidden[projectPath]
	return hidden
}

// HiddenProjects returns the excluded project paths, sorted. Used for
// reporting the current state back to the UI and for logging.
func HiddenProjects() []string {
	dataSettings.RLock()
	defer dataSettings.RUnlock()
	if len(dataSettings.hidden) == 0 {
		return nil
	}
	out := make([]string, 0, len(dataSettings.hidden))
	for p := range dataSettings.hidden {
		out = append(out, p)
	}
	sort.Strings(out)
	return out
}

// VisibleSessions drops sessions belonging to hidden projects.
//
// This is applied at Cache.List, the single point every reader goes through —
// the sessions list, the analytics endpoint and the insights summary all start
// there — so a hidden project disappears from the list and from every figure
// derived from it in one place rather than in each consumer.
func VisibleSessions(sessions []ClaudeSessionSummary) []ClaudeSessionSummary {
	dataSettings.RLock()
	hiddenCount := len(dataSettings.hidden)
	dataSettings.RUnlock()
	if hiddenCount == 0 {
		return sessions
	}

	visible := make([]ClaudeSessionSummary, 0, len(sessions))
	for _, s := range sessions {
		if !IsProjectHidden(s.ProjectPath) {
			visible = append(visible, s)
		}
	}
	return visible
}
