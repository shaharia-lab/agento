package service_test

import (
	"testing"

	"github.com/stretchr/testify/assert"

	"github.com/shaharia-lab/agento/internal/service"
)

func TestNotFoundError_Error(t *testing.T) {
	tests := []struct {
		name     string
		err      *service.NotFoundError
		expected string
	}{
		{
			name:     "typical resource",
			err:      &service.NotFoundError{Resource: "agent", ID: "my-agent"},
			expected: `agent "my-agent" not found`,
		},
		{
			name:     "different resource type",
			err:      &service.NotFoundError{Resource: "chat", ID: "abc-123"},
			expected: `chat "abc-123" not found`,
		},
		{
			name:     "empty ID",
			err:      &service.NotFoundError{Resource: "agent", ID: ""},
			expected: `agent "" not found`,
		},
		{
			name:     "empty resource",
			err:      &service.NotFoundError{Resource: "", ID: "some-id"},
			expected: ` "some-id" not found`,
		},
		{
			name:     "both empty",
			err:      &service.NotFoundError{Resource: "", ID: ""},
			expected: ` "" not found`,
		},
		{
			name:     "ID with special characters",
			err:      &service.NotFoundError{Resource: "integration", ID: "google/calendar"},
			expected: `integration "google/calendar" not found`,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.expected, tt.err.Error())
		})
	}
}

func TestNotFoundError_implements_error(t *testing.T) {
	var err error = &service.NotFoundError{Resource: "agent", ID: "x"}
	assert.Error(t, err)
}

func TestConflictError_Error(t *testing.T) {
	tests := []struct {
		name     string
		err      *service.ConflictError
		expected string
	}{
		{
			name:     "typical resource",
			err:      &service.ConflictError{Resource: "agent", ID: "my-agent"},
			expected: `agent with id "my-agent" already exists`,
		},
		{
			name:     "different resource type",
			err:      &service.ConflictError{Resource: "profile", ID: "default"},
			expected: `profile with id "default" already exists`,
		},
		{
			name:     "empty ID",
			err:      &service.ConflictError{Resource: "agent", ID: ""},
			expected: `agent with id "" already exists`,
		},
		{
			name:     "empty resource",
			err:      &service.ConflictError{Resource: "", ID: "dup"},
			expected: ` with id "dup" already exists`,
		},
		{
			name:     "both empty",
			err:      &service.ConflictError{Resource: "", ID: ""},
			expected: ` with id "" already exists`,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.expected, tt.err.Error())
		})
	}
}

func TestConflictError_implements_error(t *testing.T) {
	var err error = &service.ConflictError{Resource: "agent", ID: "x"}
	assert.Error(t, err)
}

func TestValidationError_Error(t *testing.T) {
	tests := []struct {
		name     string
		err      *service.ValidationError
		expected string
	}{
		{
			name:     "with field and message",
			err:      &service.ValidationError{Field: "name", Message: "name is required"},
			expected: `validation error for "name": name is required`,
		},
		{
			name:     "without field - returns message only",
			err:      &service.ValidationError{Field: "", Message: "invalid request body"},
			expected: "invalid request body",
		},
		{
			name:     "empty message with field",
			err:      &service.ValidationError{Field: "slug", Message: ""},
			expected: `validation error for "slug": `,
		},
		{
			name:     "both empty",
			err:      &service.ValidationError{Field: "", Message: ""},
			expected: "",
		},
		{
			name:     "field with special characters",
			err:      &service.ValidationError{Field: "config.model", Message: "unsupported model"},
			expected: `validation error for "config.model": unsupported model`,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.expected, tt.err.Error())
		})
	}
}

func TestValidationError_implements_error(t *testing.T) {
	var err error = &service.ValidationError{Field: "x", Message: "bad"}
	assert.Error(t, err)
}
