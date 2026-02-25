package server

import (
	"context"
	"fmt"
	"io/fs"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"strings"
	"time"

	"github.com/go-chi/chi/v5"
	"github.com/go-chi/chi/v5/middleware"

	"github.com/shaharia-lab/agents-platform-cc-go/internal/api"
)

// Server is the HTTP server for the agents platform.
type Server struct {
	apiServer  *api.Server
	frontendFS fs.FS // nil in dev mode
	port       int
	httpServer *http.Server
}

// New creates a new Server. Pass frontendFS=nil to proxy to Vite dev server on port 5173.
func New(apiSrv *api.Server, frontendFS fs.FS, port int) *Server {
	s := &Server{
		apiServer:  apiSrv,
		frontendFS: frontendFS,
		port:       port,
	}

	r := chi.NewRouter()
	r.Use(middleware.Recoverer)
	r.Use(middleware.RequestID)

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
		Addr:    fmt.Sprintf(":%d", port),
		Handler: r,
	}
	return s
}

// Run starts the HTTP server and blocks until ctx is cancelled.
func (s *Server) Run(ctx context.Context) error {
	ln, err := net.Listen("tcp", s.httpServer.Addr)
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
		return s.httpServer.Shutdown(shutdownCtx)
	case err := <-errCh:
		return err
	}
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
		// Trim leading slash to get the file path within the FS
		path := strings.TrimPrefix(r.URL.Path, "/")
		if path == "" {
			path = "index.html"
		}

		// Try to open the file in the embedded FS
		f, err := s.frontendFS.Open(path)
		if err != nil {
			// File not found — serve index.html for SPA client-side routing
			r2 := r.Clone(r.Context())
			r2.URL.Path = "/"
			fileServer.ServeHTTP(w, r2)
			return
		}
		f.Close()
		fileServer.ServeHTTP(w, r)
	}
}
