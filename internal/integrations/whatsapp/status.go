package whatsapp

import "sync"

// clientRegistry tracks active WhatsApp clients so their connection state
// can be queried without going through the integration registry.
type clientRegistry struct {
	mu      sync.RWMutex
	clients map[string]*Client
}

var connections = &clientRegistry{ //nolint:gochecknoglobals
	clients: make(map[string]*Client),
}

func (r *clientRegistry) register(id string, c *Client) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.clients[id] = c
}

func (r *clientRegistry) deregister(id string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	delete(r.clients, id)
}

// ConnectionStatus returns the live connection state for the WhatsApp client
// running for the given integration ID. Both values are false if no client is active.
func ConnectionStatus(id string) (connected, loggedIn bool) {
	connections.mu.RLock()
	c, ok := connections.clients[id]
	connections.mu.RUnlock()
	if !ok {
		return false, false
	}
	return c.IsConnected(), c.IsLoggedIn()
}
