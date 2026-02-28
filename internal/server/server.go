package server

import (
	"context"
	"fmt"
	"io/fs"
	"log/slog"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"strings"
	"time"

	"github.com/go-chi/chi/v5"
	"github.com/go-chi/chi/v5/middleware"
	"github.com/go-chi/cors"

	"github.com/shaharia-lab/agento/internal/api"
)

// Server is the HTTP server for the agents platform.
type Server struct {
	apiServer  *api.Server
	frontendFS fs.FS // nil in dev mode
	port       int
	logger     *slog.Logger
	httpServer *http.Server
}

// New creates a new Server. Pass frontendFS=nil to proxy to Vite dev server on port 5173.
func New(apiSrv *api.Server, frontendFS fs.FS, port int, logger *slog.Logger) *Server {
	s := &Server{
		apiServer:  apiSrv,
		frontendFS: frontendFS,
		port:       port,
		logger:     logger,
	}

	r := chi.NewRouter()
	r.Use(middleware.Recoverer)
	r.Use(middleware.RequestID)
	r.Use(s.corsMiddleware())
	r.Use(s.requestLogger)

	// Health check
	r.Get("/health", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		if _, err := w.Write([]byte(`{"status":"ok"}`)); err != nil {
			return
		}
	})

	// API routes
	r.Route("/api", func(r chi.Router) {
		apiSrv.Mount(r)
	})

	// Static files + SPA fallback
	r.Get("/*", s.spaHandler())

	s.httpServer = &http.Server{
		Addr:              fmt.Sprintf(":%d", port),
		Handler:           r,
		ReadHeaderTimeout: 10 * time.Second,
	}
	return s
}

// Run starts the HTTP server and blocks until ctx is canceled.
func (s *Server) Run(ctx context.Context) error {
	lc := &net.ListenConfig{}
	ln, err := lc.Listen(ctx, "tcp", s.httpServer.Addr)
	if err != nil {
		return fmt.Errorf("listening on %s: %w", s.httpServer.Addr, err)
	}

	errCh := make(chan error, 1)
	go func() {
		if err := s.httpServer.Serve(ln); err != nil && err != http.ErrServerClosed {
			errCh <- err
		}
	}()

	select {
	case <-ctx.Done():
		// ctx is already canceled; need a fresh context for graceful shutdown.
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second) //nolint:govet
		defer cancel()
		s.logger.Info("shutting down server")
		return s.httpServer.Shutdown(shutdownCtx)
	case err := <-errCh:
		return err
	}
}

// corsMiddleware returns a CORS middleware configured for the current mode.
//
// In dev mode (frontendFS == nil) the Vite dev server on :5173 is the only
// allowed origin. In production the frontend is embedded and served from the
// same origin as the API, so no cross-origin access is permitted at all —
// the absence of CORS headers causes browsers to block every cross-origin
// request by default.
func (s *Server) corsMiddleware() func(http.Handler) http.Handler {
	if s.frontendFS != nil {
		// Production: same-origin only — return a no-op middleware.
		return func(next http.Handler) http.Handler { return next }
	}

	// Dev mode: allow the Vite dev server origin.
	return cors.Handler(cors.Options{
		AllowedOrigins:   []string{"http://localhost:5173"},
		AllowedMethods:   []string{"GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"},
		AllowedHeaders:   []string{"Accept", "Authorization", "Content-Type"},
		AllowCredentials: false,
		MaxAge:           300,
	})
}

// requestLogger is a chi middleware that logs each incoming request.
func (s *Server) requestLogger(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		start := time.Now()
		ww := middleware.NewWrapResponseWriter(w, r.ProtoMajor)
		next.ServeHTTP(ww, r)
		s.logger.Debug("http request",
			slog.String("method", r.Method),
			slog.String("path", r.URL.Path),
			slog.Int("status", ww.Status()),
			slog.Duration("duration", time.Since(start)),
			slog.String("request_id", middleware.GetReqID(r.Context())),
		)
	})
}

// spaHandler returns an http.HandlerFunc that serves the embedded SPA (or proxies
// to the Vite dev server when frontendFS is nil).
func (s *Server) spaHandler() http.HandlerFunc {
	if s.frontendFS == nil {
		// Dev mode: proxy everything to Vite on :5173
		target, err := url.Parse("http://localhost:5173")
		if err != nil {
			s.logger.Error("failed to parse Vite dev server URL", "error", err)
			return func(w http.ResponseWriter, _ *http.Request) {
				http.Error(w, "internal server error", http.StatusInternalServerError)
			}
		}
		proxy := httputil.NewSingleHostReverseProxy(target)
		return proxy.ServeHTTP
	}

	fileServer := http.FileServer(http.FS(s.frontendFS))

	return func(w http.ResponseWriter, r *http.Request) {
		path := strings.TrimPrefix(r.URL.Path, "/")
		if path == "" {
			path = "index.html"
		}

		f, err := s.frontendFS.Open(path)
		if err != nil {
			// File not found — serve index.html for SPA client-side routing.
			r2 := r.Clone(r.Context())
			r2.URL.Path = "/"
			fileServer.ServeHTTP(w, r2)
			return
		}
		if cerr := f.Close(); cerr != nil {
			s.logger.Error("failed to close file", "path", path, "error", cerr)
		}
		fileServer.ServeHTTP(w, r)
	}
}
