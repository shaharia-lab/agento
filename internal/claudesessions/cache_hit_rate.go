package claudesessions

// CacheHitRate is the one definition of cache hit rate in the codebase.
//
// Two different formulas used to share the name. The analytics dashboard
// computed cacheRead/(input+cacheRead) and the insight pipeline computed
// cacheRead/(cacheCreation+cacheRead), so the same corpus and the same window
// produced ~100% on one page and ~74% on another. The first is pinned near 100%
// for any long conversation (cache read dwarfs fresh input by construction) and
// so carries no information; the second answers "of the tokens that touched the
// cache, how many were reads", which silently excludes uncached input and so
// flatters a model that never caches at all.
//
// It returns the read share of *every* input-side token — fresh input,
// cache writes and cache reads together. That is the only denominator under
// which a model with no prompt caching scores 0 rather than being excused, and
// under which the number moves when caching improves.
//
// Both callers (buildCacheEfficiency in analytics.go and TokenProfileProcessor
// in token_processor.go) go through this function. Changing it changes what
// stored insight rows mean, so a change here needs a CurrentProcessorVersion
// bump — the same rule isUserTurnContent carries.
func CacheHitRate(inputTokens, cacheReadTokens, cacheCreationTokens int) float64 {
	denom := inputTokens + cacheReadTokens + cacheCreationTokens
	if denom <= 0 {
		return 0
	}
	return float64(cacheReadTokens) / float64(denom)
}
