package api

import (
	"encoding/json"
	"errors"
	"net/http"
	"time"

	"github.com/shaharia-lab/agento/internal/pricing"
	"github.com/shaharia-lab/agento/internal/service"
)

// PricingRateRequest is the wire shape for creating or correcting a rate.
//
// effective_from accepts either a plain date (YYYY-MM-DD, what an
// <input type="date"> produces) or a full RFC3339 timestamp, because the UI
// sends the former and API clients tend to send the latter.
type PricingRateRequest struct {
	Provider            string  `json:"provider"`
	ModelPattern        string  `json:"model_pattern"`
	MatchType           string  `json:"match_type"`
	DisplayName         string  `json:"display_name"`
	InputPerMTok        float64 `json:"input_per_mtok"`
	OutputPerMTok       float64 `json:"output_per_mtok"`
	CacheWrite5mPerMTok float64 `json:"cache_write_5m_per_mtok"`
	CacheWrite1hPerMTok float64 `json:"cache_write_1h_per_mtok"`
	CacheReadPerMTok    float64 `json:"cache_read_per_mtok"`
	EffectiveFrom       string  `json:"effective_from"`
	Source              string  `json:"source"`
	Billable            *bool   `json:"billable"`
	Estimated           bool    `json:"estimated"`
}

// parseEffectiveFrom accepts a date-only or RFC3339 value, normalized to UTC.
// A date-only value means midnight UTC on that day.
func parseEffectiveFrom(s string) (time.Time, error) {
	if s == "" {
		return time.Time{}, errors.New("effective_from is required")
	}
	if t, err := time.Parse(time.RFC3339, s); err == nil {
		return t.UTC(), nil
	}
	t, err := time.Parse("2006-01-02", s)
	if err != nil {
		return time.Time{}, errors.New("effective_from must be YYYY-MM-DD or RFC3339")
	}
	return t.UTC(), nil
}

// toRate converts the request to a catalog rate. Billable defaults to true when
// omitted: every real model is billable, and the zero value of a Go bool would
// otherwise silently mark a priced model free.
func (req PricingRateRequest) toRate() (pricing.Rate, error) {
	from, err := parseEffectiveFrom(req.EffectiveFrom)
	if err != nil {
		return pricing.Rate{}, err
	}
	billable := req.Billable == nil || *req.Billable
	return pricing.Rate{
		Provider:            req.Provider,
		ModelPattern:        req.ModelPattern,
		MatchType:           pricing.MatchType(req.MatchType),
		DisplayName:         req.DisplayName,
		InputPerMTok:        req.InputPerMTok,
		OutputPerMTok:       req.OutputPerMTok,
		CacheWrite5mPerMTok: req.CacheWrite5mPerMTok,
		CacheWrite1hPerMTok: req.CacheWrite1hPerMTok,
		CacheReadPerMTok:    req.CacheReadPerMTok,
		EffectiveFrom:       from,
		Source:              req.Source,
		Billable:            billable,
		Estimated:           req.Estimated,
	}, nil
}

// handleGetPricingCatalog returns every model with its current rate, full rate
// history, and the models seen in sessions that still have no rate at all.
func (s *Server) handleGetPricingCatalog(w http.ResponseWriter, r *http.Request) {
	if s.pricingSvc == nil {
		s.writeError(w, http.StatusServiceUnavailable, "pricing service not configured")
		return
	}
	catalog, err := s.pricingSvc.Catalog(r.Context())
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, catalog)
}

// handleAddPricingRate appends a new effective-dated rate, leaving every
// existing rate untouched so past usage keeps the price it was charged at.
//
// A collision is not a bare 409: the conflicting row is returned so the UI can
// offer "you already have a rate from that date — correct it instead?" rather
// than making the user guess what they hit.
func (s *Server) handleAddPricingRate(w http.ResponseWriter, r *http.Request) {
	if s.pricingSvc == nil {
		s.writeError(w, http.StatusServiceUnavailable, "pricing service not configured")
		return
	}
	var req PricingRateRequest
	if json.NewDecoder(r.Body).Decode(&req) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}
	rate, err := req.toRate()
	if err != nil {
		s.writeError(w, http.StatusUnprocessableEntity, err.Error())
		return
	}

	created, err := s.pricingSvc.AddRate(r.Context(), rate)
	var conflict *service.ConflictError
	if errors.As(err, &conflict) {
		s.writeJSON(w, http.StatusConflict, map[string]any{
			"error":    err.Error(),
			"existing": created,
		})
		return
	}
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.afterRateChange()
	s.writeJSON(w, http.StatusCreated, created)
}

// handleCorrectPricingRate edits a rate in place, for a value entered in error.
// Unlike adding, this rewrites already-reported costs for that window — which
// is why it is a separate endpoint rather than an upsert.
func (s *Server) handleCorrectPricingRate(w http.ResponseWriter, r *http.Request) {
	if s.pricingSvc == nil {
		s.writeError(w, http.StatusServiceUnavailable, "pricing service not configured")
		return
	}
	var req PricingRateRequest
	if json.NewDecoder(r.Body).Decode(&req) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}
	rate, err := req.toRate()
	if err != nil {
		s.writeError(w, http.StatusUnprocessableEntity, err.Error())
		return
	}

	updated, err := s.pricingSvc.CorrectRate(r.Context(), rate)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.afterRateChange()
	s.writeJSON(w, http.StatusOK, updated)
}

// handleDeletePricingRate removes one rate. The key is a query pair rather than
// a path segment because a model pattern is not path-safe — "mixedbread-ai/"
// carries a slash and "<synthetic>" angle brackets — and because the row's
// identity is (model_pattern, effective_from), not an opaque id.
func (s *Server) handleDeletePricingRate(w http.ResponseWriter, r *http.Request) {
	if s.pricingSvc == nil {
		s.writeError(w, http.StatusServiceUnavailable, "pricing service not configured")
		return
	}
	pattern := r.URL.Query().Get("model_pattern")
	from, err := parseEffectiveFrom(r.URL.Query().Get("effective_from"))
	if err != nil {
		s.writeError(w, http.StatusUnprocessableEntity, err.Error())
		return
	}
	if err := s.pricingSvc.DeleteRate(r.Context(), pattern, from); err != nil {
		s.httpErr(w, err)
		return
	}
	s.afterRateChange()
	w.WriteHeader(http.StatusNoContent)
}

// afterRateChange invalidates the session cache so the next read re-prices
// against the edited catalog instead of serving costs computed under the old
// one. Since #188 costs are stored per session, so without this the change
// would not surface until the hourly TTL expired.
func (s *Server) afterRateChange() {
	if s.claudeSessionCache != nil {
		s.claudeSessionCache.Invalidate()
	}
}
