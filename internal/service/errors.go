package service

import "fmt"

// NotFoundError is returned when a requested resource does not exist.
type NotFoundError struct {
	Resource string
	ID       string
}

func (e *NotFoundError) Error() string {
	return fmt.Sprintf("%s %q not found", e.Resource, e.ID)
}

// ConflictError is returned when a resource with the same identifier already exists.
type ConflictError struct {
	Resource string
	ID       string
}

func (e *ConflictError) Error() string {
	return fmt.Sprintf("%s with id %q already exists", e.Resource, e.ID)
}

// ValidationError is returned when request data fails validation.
type ValidationError struct {
	Field   string
	Message string
}

func (e *ValidationError) Error() string {
	if e.Field != "" {
		return fmt.Sprintf("validation error for %q: %s", e.Field, e.Message)
	}
	return e.Message
}
