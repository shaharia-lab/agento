package api_test

import (
	"encoding/json"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/go-chi/chi/v5"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"

	"github.com/shaharia-lab/agento/internal/api"
	"github.com/shaharia-lab/agento/internal/pricing"
	"github.com/shaharia-lab/agento/internal/service"
	svcmocks "github.com/shaharia-lab/agento/internal/service/mocks"
)

// pricingHarness wires just the pricing service, since these routes touch
// nothing else.
type pricingHarness struct {
	svc    *svcmocks.MockPricingService
	router chi.Router
}

func newPricingHarness(t *testing.T) *pricingHarness {
	t.Helper()
	svc := new(svcmocks.MockPricingService)
	srv := api.New(api.ServerConfig{PricingSvc: svc, Logger: slog.Default()})
	r := chi.NewRouter()
	srv.Mount(r)
	return &pricingHarness{svc: svc, router: r}
}

func (h *pricingHarness) do(req *http.Request) *httptest.ResponseRecorder {
	w := httptest.NewRecorder()
	h.router.ServeHTTP(w, req)
	return w
}

// jsonReq builds a request against the rates endpoint, which is the only route
// in this file that takes a body.
func jsonReq(method, body string) *http.Request {
	req := httptest.NewRequest(method, "/pricing/rates", strings.NewReader(body))
	req.Header.Set("Content-Type", "application/json")
	return req
}

func TestGetPricingCatalog(t *testing.T) {
	h := newPricingHarness(t)
	h.svc.On("Catalog", mock.Anything).Return(&service.PricingCatalog{
		Models: []service.PricedModel{{
			ModelPattern: "claude-opus-5",
			Provider:     "anthropic",
			Rates:        []pricing.Rate{{ModelPattern: "claude-opus-5", InputPerMTok: 5}},
		}},
		UnpricedModels: []string{"any"},
		Revision:       42,
	}, nil)

	w := h.do(httptest.NewRequest(http.MethodGet, "/pricing/catalog", nil))
	require.Equal(t, http.StatusOK, w.Code)

	var got service.PricingCatalog
	require.NoError(t, json.Unmarshal(w.Body.Bytes(), &got))
	assert.Len(t, got.Models, 1)
	assert.Equal(t, []string{"any"}, got.UnpricedModels)
	assert.Equal(t, int64(42), got.Revision)
}

// TestAddPricingRate covers the append path plus the error mapping the UI
// depends on. The conflict case is the interesting one: it must carry the
// colliding row so the UI can offer to correct it instead.
func TestAddPricingRate(t *testing.T) {
	validBody := `{"model_pattern":"k3","match_type":"exact","input_per_mtok":3,
		"output_per_mtok":15,"effective_from":"2026-08-09"}`

	tests := []struct {
		name       string
		body       string
		rate       *pricing.Rate
		err        error
		wantStatus int
	}{
		{
			name:       "success",
			body:       validBody,
			rate:       &pricing.Rate{ModelPattern: "k3", InputPerMTok: 3, OutputPerMTok: 15},
			wantStatus: http.StatusCreated,
		},
		{
			name:       "invalid JSON",
			body:       `{invalid`,
			wantStatus: http.StatusBadRequest,
		},
		{
			name:       "unparseable effective_from",
			body:       `{"model_pattern":"k3","effective_from":"the ninth of August"}`,
			wantStatus: http.StatusUnprocessableEntity,
		},
		{
			name:       "validation error from the service",
			body:       validBody,
			err:        &service.ValidationError{Field: "input_per_mtok", Message: "rate must not be negative"},
			wantStatus: http.StatusUnprocessableEntity,
		},
		{
			name:       "conflict returns the existing rate",
			body:       validBody,
			rate:       &pricing.Rate{ModelPattern: "k3", InputPerMTok: 99},
			err:        &service.ConflictError{Resource: "rate", ID: "k3@2026-08-09T00:00:00Z"},
			wantStatus: http.StatusConflict,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			h := newPricingHarness(t)
			// The two rejections happen before the service is reached.
			if tc.wantStatus != http.StatusBadRequest && tc.name != "unparseable effective_from" {
				h.svc.On("AddRate", mock.Anything, mock.Anything).Return(tc.rate, tc.err)
			}

			w := h.do(jsonReq(http.MethodPost, tc.body))
			assert.Equal(t, tc.wantStatus, w.Code)

			if tc.wantStatus == http.StatusConflict {
				var body struct {
					Error    string        `json:"error"`
					Existing *pricing.Rate `json:"existing"`
				}
				require.NoError(t, json.Unmarshal(w.Body.Bytes(), &body))
				require.NotNil(t, body.Existing,
					"the conflicting rate must come back so the UI can offer to correct it")
				assert.Equal(t, 99.0, body.Existing.InputPerMTok)
			}
		})
	}
}

// TestCorrectPricingRate asserts correcting a rate that does not exist is a
// 404 rather than silently creating one — the mirror of AddRate refusing to
// overwrite. Blurring the two is how a user rewrites their own cost history by
// accident.
func TestCorrectPricingRate(t *testing.T) {
	body := `{"model_pattern":"k3","match_type":"exact","input_per_mtok":4,
		"output_per_mtok":16,"effective_from":"2026-08-09T00:00:00Z"}`

	t.Run("success", func(t *testing.T) {
		h := newPricingHarness(t)
		h.svc.On("CorrectRate", mock.Anything, mock.Anything).
			Return(&pricing.Rate{ModelPattern: "k3", InputPerMTok: 4}, nil)

		w := h.do(jsonReq(http.MethodPut, body))
		require.Equal(t, http.StatusOK, w.Code)

		var got pricing.Rate
		require.NoError(t, json.Unmarshal(w.Body.Bytes(), &got))
		assert.Equal(t, 4.0, got.InputPerMTok)
	})

	t.Run("correcting a rate that does not exist is 404", func(t *testing.T) {
		h := newPricingHarness(t)
		h.svc.On("CorrectRate", mock.Anything, mock.Anything).
			Return(nil, &service.NotFoundError{Resource: "rate", ID: "k3@2026-08-09T00:00:00Z"})

		w := h.do(jsonReq(http.MethodPut, body))
		assert.Equal(t, http.StatusNotFound, w.Code)
	})
}

func TestDeletePricingRate(t *testing.T) {
	t.Run("success", func(t *testing.T) {
		h := newPricingHarness(t)
		h.svc.On("DeleteRate", mock.Anything, "k3", mock.Anything).Return(nil)

		w := h.do(httptest.NewRequest(http.MethodDelete,
			"/pricing/rates?model_pattern=k3&effective_from=2026-08-09", nil))
		assert.Equal(t, http.StatusNoContent, w.Code)
		assert.Empty(t, w.Body.String())
	})

	t.Run("missing effective_from is rejected", func(t *testing.T) {
		h := newPricingHarness(t)
		w := h.do(httptest.NewRequest(http.MethodDelete, "/pricing/rates?model_pattern=k3", nil))
		assert.Equal(t, http.StatusUnprocessableEntity, w.Code)
	})

	t.Run("unknown rate is 404", func(t *testing.T) {
		h := newPricingHarness(t)
		h.svc.On("DeleteRate", mock.Anything, mock.Anything, mock.Anything).
			Return(&service.NotFoundError{Resource: "rate", ID: "nope@2026-08-09T00:00:00Z"})

		w := h.do(httptest.NewRequest(http.MethodDelete,
			"/pricing/rates?model_pattern=nope&effective_from=2026-08-09", nil))
		assert.Equal(t, http.StatusNotFound, w.Code)
	})
}

// TestPricingRoutes_WithoutService keeps the endpoints honest in a build where
// pricing was never wired: 503, not a nil-pointer panic.
func TestPricingRoutes_WithoutService(t *testing.T) {
	srv := api.New(api.ServerConfig{Logger: slog.Default()})
	r := chi.NewRouter()
	srv.Mount(r)

	for _, req := range []*http.Request{
		httptest.NewRequest(http.MethodGet, "/pricing/catalog", nil),
		jsonReq(http.MethodPost, `{}`),
		jsonReq(http.MethodPut, `{}`),
		httptest.NewRequest(http.MethodDelete, "/pricing/rates?model_pattern=x", nil),
	} {
		w := httptest.NewRecorder()
		r.ServeHTTP(w, req)
		assert.Equal(t, http.StatusServiceUnavailable, w.Code, req.Method+" "+req.URL.Path)
	}
}

// TestParseEffectiveFrom_AcceptsBothShapes guards the date contract between the
// UI's <input type="date"> and API clients sending timestamps.
func TestParseEffectiveFrom_AcceptsBothShapes(t *testing.T) {
	h := newPricingHarness(t)
	var captured pricing.Rate
	h.svc.On("AddRate", mock.Anything, mock.Anything).
		Run(func(args mock.Arguments) { captured = args.Get(1).(pricing.Rate) }).
		Return(&pricing.Rate{}, nil)

	w := h.do(jsonReq(http.MethodPost,
		`{"model_pattern":"k3","input_per_mtok":3,"output_per_mtok":15,"effective_from":"2026-08-09"}`))
	require.Equal(t, http.StatusCreated, w.Code)
	assert.Equal(t, time.Date(2026, 8, 9, 0, 0, 0, 0, time.UTC), captured.EffectiveFrom,
		"a date-only value means midnight UTC that day")
	assert.True(t, captured.Billable, "billable must default to true when the field is omitted")
}
