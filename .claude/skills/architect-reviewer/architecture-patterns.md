# Architecture Patterns Reference

## SOLID Principles

- **Single Responsibility:** Each module/struct/function has one reason to change
- **Open/Closed:** Open for extension, closed for modification — use interfaces
- **Liskov Substitution:** Implementations must be substitutable for their interfaces
- **Interface Segregation:** Small, focused interfaces — not kitchen-sink interfaces
- **Dependency Inversion:** Depend on abstractions (interfaces), not concrete types

## Patterns to Verify

### Dependency Injection
- Are dependencies injected via constructors, not created internally?
- Are interfaces used at module boundaries?
- Is there a clear composition root (main/cmd)?

### Repository / Storage Pattern
- Is data access abstracted behind interfaces?
- Are storage implementations swappable?
- Is business logic free of storage-specific code?

### Service Layer
- Is business logic separated from HTTP handlers?
- Do services depend on interfaces, not concrete storage?
- Are services testable without spinning up infrastructure?

### Middleware / Decorator
- Are cross-cutting concerns (logging, auth, recovery) handled via middleware?
- Is middleware composable and single-purpose?

### Factory / Builder
- Are complex objects created consistently?
- Is construction logic separated from business logic?

## Anti-Patterns to Flag

### God Object
A struct/class/package that does too much. Signs:
- >10 methods, >5 dependencies, >300 lines
- Name is vague ("Manager", "Handler", "Processor", "Utils")

### Service Locator
Global registry that provides dependencies at runtime instead of injection. Signs:
- `GetService()`, `Resolve()`, global maps of interfaces
- Makes dependencies implicit and testing harder

### Anemic Domain Model
Data structs with no behavior — all logic lives in separate service functions.
- Not always bad in Go, but flag if domain logic is scattered

### Spaghetti Architecture
No clear layering — handlers call storage directly, business logic in HTTP layer.
- Check: can you describe the dependency flow in one sentence?

### Lava Flow
Dead code, unused types, commented-out blocks left from previous iterations.
- Check: are all exported symbols actually used?

### Shotgun Surgery
One logical change requires touching many unrelated files.
- Check: are related concerns co-located?

### Feature Envy
A function that accesses data from another module more than its own.
- Signs: excessive cross-package imports for data access

## Go-Specific Patterns

### Interface Compliance
- Interfaces defined where consumed, not where implemented
- Small interfaces (1-3 methods preferred)
- Accept interfaces, return structs

### Error Handling
- Errors wrapped with context (`fmt.Errorf("doing X: %w", err)`)
- Sentinel errors or typed errors for expected failure cases
- No swallowed errors (empty `if err != nil` blocks)

### Goroutine Safety
- Shared mutable state protected by mutex or channels
- Goroutines have clear lifecycle (start, stop, cleanup)
- Context propagation for cancellation

### Package Design
- No circular imports
- Internal packages for implementation details
- Clear public API surface per package
