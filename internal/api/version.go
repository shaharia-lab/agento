package api

import (
	"net/http"

	"github.com/shaharia-lab/agento/internal/build"
)

func (s *Server) handleVersion(w http.ResponseWriter, r *http.Request) {
	s.writeJSON(w, http.StatusOK, map[string]string{
		"version":    build.Version,
		"commit":     build.CommitSHA,
		"build_date": build.BuildDate,
	})
}
