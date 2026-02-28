# Architecture Audit Checklist

## Project Structure

- [ ] Clear top-level organization (cmd, internal, pkg, frontend, etc.)
- [ ] Logical grouping of related code
- [ ] Consistent directory naming
- [ ] No orphaned or misplaced files
- [ ] Build and config files at root level

## Dependency Flow

- [ ] Dependencies flow in one direction (handlers → services → storage)
- [ ] No circular imports
- [ ] Shared types in a common package (not duplicated)
- [ ] External dependencies isolated behind interfaces
- [ ] Clear boundary between internal and external code

## API Design

- [ ] Consistent URL patterns and naming
- [ ] Consistent request/response formats
- [ ] Proper HTTP status codes
- [ ] Input validation at handler level
- [ ] Error responses are structured and informative
- [ ] No business logic in handlers (delegated to services)

## Service Layer

- [ ] Services define clear interfaces
- [ ] Business logic is in services, not handlers or storage
- [ ] Services are independently testable
- [ ] Cross-service communication goes through interfaces
- [ ] No direct database/storage calls from handlers

## Storage / Data Layer

- [ ] Storage implementations behind interfaces
- [ ] Data models separate from API models where needed
- [ ] Consistent serialization strategy
- [ ] Proper cleanup of resources (close files, connections)
- [ ] Error handling for I/O operations

## Error Handling

- [ ] Consistent error handling strategy across the codebase
- [ ] Errors wrapped with context at each layer
- [ ] Typed/sentinel errors for expected failure modes
- [ ] HTTP error mapping is centralized
- [ ] No swallowed errors
- [ ] No panic for recoverable conditions

## Configuration

- [ ] Config loaded in one place, passed via dependency injection
- [ ] Environment variables documented
- [ ] Defaults are sensible
- [ ] No hardcoded values that should be configurable
- [ ] Secrets not committed to source

## Testing

- [ ] Unit tests for business logic
- [ ] Integration tests for storage/external boundaries
- [ ] Test helpers avoid duplication
- [ ] Tests are independent (no shared mutable state)
- [ ] Critical paths have coverage
- [ ] Test names describe behavior, not implementation

## Frontend (if applicable)

- [ ] Component responsibilities are clear
- [ ] State management is consistent
- [ ] API calls centralized in one layer
- [ ] Types shared with backend or mirrored correctly
- [ ] No business logic in UI components
- [ ] Error states handled in UI
- [ ] Loading states handled in UI

## Security

- [ ] No hardcoded secrets or API keys
- [ ] Input validated at system boundaries
- [ ] Authentication checks on protected routes
- [ ] Authorization checks before data access
- [ ] CORS configured appropriately
- [ ] File paths validated (no path traversal)
- [ ] Dependencies scanned for vulnerabilities

## Performance

- [ ] No unbounded queries or data loading
- [ ] Pagination for list endpoints
- [ ] Resources properly closed/released
- [ ] No unnecessary allocations in loops
- [ ] Caching where repeated computation exists
- [ ] Goroutines have proper lifecycle management

## Documentation

- [ ] Architecture is documented (or self-evident from structure)
- [ ] Non-obvious decisions have comments explaining "why"
- [ ] Public APIs have documentation
- [ ] Setup instructions are accurate and complete
- [ ] No stale documentation that contradicts code

## Code Consistency

- [ ] Consistent naming conventions (camelCase, snake_case per language)
- [ ] Consistent file organization within packages
- [ ] Consistent patterns for similar operations
- [ ] Linting rules enforced and passing
- [ ] Formatting is automated and consistent
