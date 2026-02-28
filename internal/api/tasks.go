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
		httpErr(w, err)
		return
	}
	writeJSON(w, http.StatusOK, tasks)
}

// handleCreateTask creates a new scheduled task.
func (s *Server) handleCreateTask(w http.ResponseWriter, r *http.Request) {
	var task storage.ScheduledTask
	if err := json.NewDecoder(r.Body).Decode(&task); err != nil {
		writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	created, err := s.taskSvc.CreateTask(r.Context(), &task)
	if err != nil {
		httpErr(w, err)
		return
	}

	// Schedule the task if the scheduler is available.
	if s.scheduler != nil && created.Status == storage.TaskStatusActive {
		if schedErr := s.scheduler.ScheduleTask(created); schedErr != nil {
			s.logger.Warn("failed to schedule newly created task",
				"task_id", created.ID, "error", schedErr)
		}
	}

	writeJSON(w, http.StatusCreated, created)
}

// handleGetTask returns a single task by ID.
func (s *Server) handleGetTask(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	task, err := s.taskSvc.GetTask(r.Context(), id)
	if err != nil {
		httpErr(w, err)
		return
	}
	writeJSON(w, http.StatusOK, task)
}

// handleUpdateTask updates an existing task.
func (s *Server) handleUpdateTask(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	var task storage.ScheduledTask
	if err := json.NewDecoder(r.Body).Decode(&task); err != nil {
		writeError(w, http.StatusBadRequest, errInvalidJSONBody)
		return
	}

	updated, err := s.taskSvc.UpdateTask(r.Context(), id, &task)
	if err != nil {
		httpErr(w, err)
		return
	}

	// Reschedule the task.
	if s.scheduler != nil {
		s.scheduler.UnscheduleTask(id)
		if updated.Status == storage.TaskStatusActive {
			if schedErr := s.scheduler.ScheduleTask(updated); schedErr != nil {
				s.logger.Warn("failed to reschedule updated task",
					"task_id", id, "error", schedErr)
			}
		}
	}

	writeJSON(w, http.StatusOK, updated)
}

// handleDeleteTask deletes a task and its job history.
func (s *Server) handleDeleteTask(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	// Unschedule first.
	if s.scheduler != nil {
		s.scheduler.UnscheduleTask(id)
	}

	if err := s.taskSvc.DeleteTask(r.Context(), id); err != nil {
		httpErr(w, err)
		return
	}

	w.WriteHeader(http.StatusNoContent)
}

// handlePauseTask pauses a scheduled task.
func (s *Server) handlePauseTask(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	task, err := s.taskSvc.PauseTask(r.Context(), id)
	if err != nil {
		httpErr(w, err)
		return
	}

	if s.scheduler != nil {
		s.scheduler.UnscheduleTask(id)
	}

	writeJSON(w, http.StatusOK, task)
}

// handleResumeTask resumes a paused task.
func (s *Server) handleResumeTask(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")

	task, err := s.taskSvc.ResumeTask(r.Context(), id)
	if err != nil {
		httpErr(w, err)
		return
	}

	if s.scheduler != nil {
		if schedErr := s.scheduler.ScheduleTask(task); schedErr != nil {
			s.logger.Warn("failed to schedule resumed task",
				"task_id", id, "error", schedErr)
		}
	}

	writeJSON(w, http.StatusOK, task)
}

// handleListTaskJobHistory returns job history for a specific task.
func (s *Server) handleListTaskJobHistory(w http.ResponseWriter, r *http.Request) {
	taskID := chi.URLParam(r, "id")
	limit := parseQueryInt(r, "limit", 50)

	history, err := s.taskSvc.ListJobHistory(r.Context(), taskID, limit)
	if err != nil {
		httpErr(w, err)
		return
	}
	writeJSON(w, http.StatusOK, history)
}

// handleListAllJobHistory returns all job history entries with pagination.
func (s *Server) handleListAllJobHistory(w http.ResponseWriter, r *http.Request) {
	limit := parseQueryInt(r, "limit", 50)
	offset := parseQueryInt(r, "offset", 0)

	history, err := s.taskSvc.ListAllJobHistory(r.Context(), limit, offset)
	if err != nil {
		httpErr(w, err)
		return
	}
	writeJSON(w, http.StatusOK, history)
}

// handleGetJobHistory returns a single job history entry.
func (s *Server) handleGetJobHistory(w http.ResponseWriter, r *http.Request) {
	id := chi.URLParam(r, "id")
	jh, err := s.taskSvc.GetJobHistory(r.Context(), id)
	if err != nil {
		httpErr(w, err)
		return
	}
	writeJSON(w, http.StatusOK, jh)
}

func parseQueryInt(r *http.Request, key string, defaultVal int) int {
	s := r.URL.Query().Get(key)
	if s == "" {
		return defaultVal
	}
	v, err := strconv.Atoi(s)
	if err != nil || v < 0 {
		return defaultVal
	}
	return v
}
