package claudesessions

import (
	"container/list"
	"fmt"
	"strings"
	"sync"
	"time"
)

// Memoization for the analytics report.
//
// Every request rebuilt the whole thing from a full corpus load: Cache.List
// reads claude_session_cache with its grouped sub-agent aggregate plus two more
// full-table reads, and AggregateAnalytics then walks the result about a dozen
// times — summary, time series, cache efficiency, model breakdown, cost by
// model, cost over time twice, project breakdown, project activity, top
// sessions, sessions per model, most active days, heatmap, hourly activity.
// Nothing was reused. The dashboards make that worse than it sounds: the
// general-usage and token-usage pages each fire two requests (current window
// and the previous one to compare against) and the insights page fires three,
// so opening a dashboard was five to seven full corpus aggregations.
//
// It is memoized rather than made incremental because the inputs that can
// change it are all already recorded: the scan timestamp, the pricing revision
// and the idle threshold live in claude_cache_metadata precisely so stored
// figures can be invalidated, and the hidden-project set is process state. A
// key built from those is exact — there is no window in which a stale report
// can be served, because anything that would change one moves the key.

// analyticsCacheSize is how many distinct (window, project, timezone) reports
// are kept. Twenty covers a user moving between dashboards and toggling
// comparison windows without evicting what they came back to; the reports are
// tens to hundreds of kilobytes, so the ceiling is a few megabytes.
const analyticsCacheSize = 20

// analyticsMemo is a small LRU of built reports keyed by request and by every
// input that can change one.
type analyticsMemo struct {
	mu      sync.Mutex
	entries map[string]*list.Element
	order   *list.List // front = most recently used
}

type memoEntry struct {
	key    string
	report AnalyticsReport
}

func newAnalyticsMemo() *analyticsMemo {
	return &analyticsMemo{
		entries: map[string]*list.Element{},
		order:   list.New(),
	}
}

func (m *analyticsMemo) get(key string) (AnalyticsReport, bool) {
	m.mu.Lock()
	defer m.mu.Unlock()
	el, ok := m.entries[key]
	if !ok {
		return AnalyticsReport{}, false
	}
	m.order.MoveToFront(el)
	return el.Value.(*memoEntry).report, true
}

func (m *analyticsMemo) put(key string, report AnalyticsReport) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if el, ok := m.entries[key]; ok {
		el.Value.(*memoEntry).report = report
		m.order.MoveToFront(el)
		return
	}
	m.entries[key] = m.order.PushFront(&memoEntry{key: key, report: report})
	for m.order.Len() > analyticsCacheSize {
		oldest := m.order.Back()
		if oldest == nil {
			return
		}
		m.order.Remove(oldest)
		delete(m.entries, oldest.Value.(*memoEntry).key)
	}
}

// analyticsCacheKey identifies a report by the request and by every input that
// can change what the request answers.
//
// Getting this wrong in either direction is a real failure: too coarse serves a
// stale report after a rate edit, too fine makes the memo never hit. The four
// invalidation inputs are the same ones the scan already tracks — a rate edit
// moves pricingRev and forces a re-read, a threshold change moves
// idleThresholdMs and does the same, and any re-read moves lastScanned — plus
// the hidden-project set, which is process state rather than cached state and
// therefore has to be fingerprinted here.
type analyticsCacheKey struct {
	from, to        time.Time
	project         string
	tz              string
	lastScanned     time.Time
	pricingRev      int64
	idleThresholdMs int64
	hidden          string
}

func (k analyticsCacheKey) String() string {
	return fmt.Sprintf("%d|%d|%s|%s|%d|%d|%d|%s",
		k.from.UnixNano(), k.to.UnixNano(), k.project, k.tz,
		k.lastScanned.UnixNano(), k.pricingRev, k.idleThresholdMs, k.hidden)
}

// Analytics returns the report for p, from the memo when nothing that could
// change it has moved.
//
// It also triggers the same background rescan List does, so opening a dashboard
// after a rate edit starts the re-cost rather than waiting for someone to open
// the sessions list.
//
// The returned report **shares its slices and maps with the memo**. Every
// caller today marshals it and nothing more, which is why it is handed back
// rather than deep-copied — copying a report per cache hit would spend most of
// what memoizing it saved. A caller that needs to mutate one must copy first.
func (c *Cache) Analytics(p AnalyticsParams) AnalyticsReport {
	c.ensureFresh()

	key := analyticsCacheKey{
		from:            p.From,
		to:              p.To,
		project:         p.Project,
		tz:              p.location().String(),
		lastScanned:     c.LastScannedAt(),
		pricingRev:      currentPricingRevision(),
		idleThresholdMs: IdleGapThreshold().Milliseconds(),
		hidden:          strings.Join(HiddenProjects(), "\x00"),
	}.String()

	if report, ok := c.analytics.get(key); ok {
		return report
	}
	report := AggregateAnalytics(c.loadOrEmpty(), p)
	c.analytics.put(key, report)
	return report
}
