# Code Smells Checklist

## Function-Level Smells

- [ ] Long functions (>50 lines — investigate, >100 lines — always flag)
- [ ] Multiple responsibilities in one function
- [ ] Too many parameters (>4)
- [ ] Boolean flag parameters (split into two functions instead)
- [ ] Deep nesting (>3 levels of indentation)
- [ ] Output parameters (returning via pointer args instead of return values)
- [ ] Functions that mix abstraction levels (high-level orchestration + low-level details)
- [ ] Inconsistent return patterns (sometimes error, sometimes panic, sometimes log-and-continue)

## Struct / Type-Level Smells

- [ ] Large structs (>10 fields — may need decomposition)
- [ ] Too many methods on one type
- [ ] Struct used as a grab-bag (unrelated fields grouped together)
- [ ] Exported fields that should be private
- [ ] Missing zero-value safety (struct usable without constructor?)
- [ ] Feature envy (methods that mostly use another type's data)

## Package-Level Smells

- [ ] Circular dependencies between packages
- [ ] Too many files in one package (>15 — consider splitting)
- [ ] Package name doesn't reflect contents
- [ ] `utils`, `helpers`, `common` packages (usually a sign of poor organization)
- [ ] Mixed abstraction levels in one package
- [ ] Package exports more than it should

## Naming Smells

- [ ] Inconsistent naming conventions across packages
- [ ] Abbreviations without context (`cfg`, `mgr`, `ctx` used inconsistently)
- [ ] Names that lie (function name doesn't match what it does)
- [ ] Generic names (`data`, `result`, `item`, `temp`) in non-trivial scope
- [ ] Stuttering (`user.UserName` instead of `user.Name`)

## Duplication Smells

- [ ] Copy-pasted code blocks (>5 lines identical)
- [ ] Similar functions that differ in one parameter (should be parameterized)
- [ ] Repeated error handling patterns that should be middleware
- [ ] Duplicated validation logic across handlers
- [ ] Same struct defined in multiple places

## Error Handling Smells

- [ ] Swallowed errors (`_ = doSomething()` or empty error blocks)
- [ ] Errors without context (`return err` instead of wrapping)
- [ ] Panic used for recoverable errors
- [ ] Inconsistent error types (string errors vs typed errors vs sentinel)
- [ ] Error messages that don't help debugging (no context about what failed)
- [ ] Logging an error AND returning it (double-reporting)

## Test Smells

- [ ] Tests coupled to implementation details (break on refactor)
- [ ] Missing edge case coverage
- [ ] Test names don't describe the scenario
- [ ] Test setup is >50% of the test function
- [ ] Shared mutable test state between test cases
- [ ] No table-driven tests where patterns repeat
- [ ] Tests that pass but don't actually assert anything meaningful

## Concurrency Smells (Go-specific)

- [ ] Goroutines without lifecycle management (fire-and-forget)
- [ ] Shared state without synchronization
- [ ] Channel operations without timeout/context
- [ ] Mutex held across I/O operations
- [ ] Race conditions (check with `go test -race`)

## Dependency Smells

- [ ] Concrete types where interfaces would allow testing
- [ ] Dependencies created inside functions instead of injected
- [ ] Too many dependencies in one constructor (>5 — struct does too much)
- [ ] Unused dependencies (imported but not meaningfully used)
- [ ] Pinned to old versions without reason
