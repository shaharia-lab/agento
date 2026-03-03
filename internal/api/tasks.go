package api

import (
	"encoding/json"
	"net/http"
	"strconv"

	"github.com/go-chi/chi/v5"

	"github.com/shaharia-lab/agento/internal/storage"
)

// handleListTasks returns all scheduled tasks.
func (s *Server) handleListTasks(w http.ResponseWriter, r *http.Request) {
	tasks, err := s.taskSvc.ListTasks(r.Context())
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, tasks)
}

// handleCreateTask creates a new scheduled task and schedules it if active.
func (s *Server) handleCreateTask(w http.ResponseWriter, r *http.Request) {
	var req CreateTaskRequest
	if json.NewDecoder(r.Body).Decode(&req) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	task := &storage.ScheduledTask{
		Name:           req.Name,
		Description:    req.Description,
		AgentSlug:      req.AgentSlug,
		Prompt:         req.Prompt,
		ScheduleType:   req.ScheduleType,
		ScheduleConfig: req.ScheduleConfig,
		Status:         req.Status,
		TimeoutMinutes: req.TimeoutMinutes,
		SaveOutput:     req.SaveOutput,
	}

	created, err := s.taskSvc.CreateTask(r.Context(), task)
	if err != nil {
		s.httpErr(w, err)
		return
	}

	s.writeJSON(w, http.StatusCreated, created)
}

// handleGetTask returns a single task by ID.
func (s *Server) handleGetTask(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	task, err := s.taskSvc.GetTask(r.Context(), id)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, task)
}

// handleUpdateTask updates an existing task and reschedules it.
func (s *Server) handleUpdateTask(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	var req UpdateTaskRequest
	if json.NewDecoder(r.Body).Decode(&req) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	task := &storage.ScheduledTask{
		Name:           req.Name,
		Description:    req.Description,
		AgentSlug:      req.AgentSlug,
		Prompt:         req.Prompt,
		ScheduleType:   req.ScheduleType,
		ScheduleConfig: req.ScheduleConfig,
		Status:         req.Status,
		TimeoutMinutes: req.TimeoutMinutes,
		SaveOutput:     req.SaveOutput,
	}

	updated, err := s.taskSvc.UpdateTask(r.Context(), id, task)
	if err != nil {
		s.httpErr(w, err)
		return
	}

	s.writeJSON(w, http.StatusOK, updated)
}

// handleDeleteTask deletes a task and its job history, and unschedules it.
func (s *Server) handleDeleteTask(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if err := s.taskSvc.DeleteTask(r.Context(), id); err != nil {
		s.httpErr(w, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

// handlePauseTask pauses a scheduled task and removes it from the scheduler.
func (s *Server) handlePauseTask(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	task, err := s.taskSvc.PauseTask(r.Context(), id)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, task)
}

// handleResumeTask resumes a paused task and re-adds it to the scheduler.
func (s *Server) handleResumeTask(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	task, err := s.taskSvc.ResumeTask(r.Context(), id)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, task)
}

// handleListTaskJobHistory returns job history for a specific task.
func (s *Server) handleListTaskJobHistory(w http.ResponseWriter, r *http.Request) {
	taskID := chi.URLParam(r, "id")
	limit := parseQueryInt(r, "limit", 50)

	history, err := s.taskSvc.ListJobHistory(r.Context(), taskID, limit)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, history)
}

// handleListAllJobHistory returns all job history entries with pagination.
func (s *Server) handleListAllJobHistory(w http.ResponseWriter, r *http.Request) {
	limit := parseQueryInt(r, "limit", 50)
	offset := parseQueryInt(r, "offset", 0)

	history, err := s.taskSvc.ListAllJobHistory(r.Context(), limit, offset)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, history)
}

// handleGetJobHistory returns a single job history entry.
func (s *Server) handleGetJobHistory(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	jh, err := s.taskSvc.GetJobHistory(r.Context(), id)
	if err != nil {
		s.httpErr(w, err)
		return
	}
	s.writeJSON(w, http.StatusOK, jh)
}

// handleDeleteJobHistory deletes a single job history entry.
func (s *Server) handleDeleteJobHistory(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	if err := s.taskSvc.DeleteJobHistory(r.Context(), id); err != nil {
		s.httpErr(w, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

// handleBulkDeleteJobHistory deletes multiple job history entries.
func (s *Server) handleBulkDeleteJobHistory(w http.ResponseWriter, r *http.Request) {
	var req BulkDeleteRequest
	if json.NewDecoder(r.Body).Decode(&req) != nil {
		s.writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}
	if len(req.IDs) == 0 {
		s.writeError(w, http.StatusBadRequest, "ids must not be empty")
		return
	}
	if len(req.IDs) > maxQueryLimit {
		s.writeError(w, http.StatusBadRequest, "too many ids (max 500)")
		return
	}
	if err := s.taskSvc.BulkDeleteJobHistory(r.Context(), req.IDs); err != nil {
		s.httpErr(w, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

// maxQueryLimit is the maximum number of records that may be requested in a
// single paginated query, preventing resource exhaustion from large limit values.
const maxQueryLimit = 500

func parseQueryInt(r *http.Request, key string, defaultVal int) int {
	s := r.URL.Query().Get(key)
	if s == "" {
		return defaultVal
	}
	v, err := strconv.Atoi(s)
	if err != nil || v < 0 {
		return defaultVal
	}
	if v > maxQueryLimit {
		return maxQueryLimit
	}
	return v
}
