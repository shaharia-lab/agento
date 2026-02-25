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
	r.Use(s.requestLogger)

	// Health check
	r.Get("/health", func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(`{"status":"ok"}`))
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
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		s.logger.Info("shutting down server")
		return s.httpServer.Shutdown(shutdownCtx)
	case err := <-errCh:
		return err
	}
}

// requestLogger is a chi middleware that logs each incoming request.
func (s *Server) requestLogger(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		start := time.Now()
		ww := middleware.NewWrapResponseWriter(w, r.ProtoMajor)
		next.ServeHTTP(ww, r)
		s.logger.Info("http request",
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
		target, _ := url.Parse("http://localhost:5173")
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
		_ = f.Close()
		fileServer.ServeHTTP(w, r)
	}
}
