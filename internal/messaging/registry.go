package messaging

import "sync"

// platformRegistry is a thread-safe map of Platform instances keyed by their ID.
type platformRegistry struct {
	mu        sync.RWMutex
	platforms map[string]Platform
}

func newPlatformRegistry() *platformRegistry {
	return &platformRegistry{
		platforms: make(map[string]Platform),
	}
}

func (r *platformRegistry) put(id string, p Platform) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.platforms[id] = p
}

func (r *platformRegistry) get(id string) (Platform, bool) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	p, ok := r.platforms[id]
	return p, ok
}

func (r *platformRegistry) delete(id string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	delete(r.platforms, id)
}

func (r *platformRegistry) all() map[string]Platform {
	r.mu.RLock()
	defer r.mu.RUnlock()
	out := make(map[string]Platform, len(r.platforms))
	for id, p := range r.platforms {
		out[id] = p
	}
	return out
}
