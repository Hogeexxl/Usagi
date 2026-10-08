package storage

import (
	"context"
	"sync"
)

type RevisionTuple struct {
	DataRevision   int64
	StatusRevision int64
}

type revisionHub struct {
	mu          sync.Mutex
	current     RevisionTuple
	subscribers map[uint64]chan RevisionTuple
	nextID      uint64
	closed      bool
	done        chan struct{}
}

func newRevisionHub(initial RevisionTuple) *revisionHub {
	return &revisionHub{
		current:     initial,
		subscribers: make(map[uint64]chan RevisionTuple),
		done:        make(chan struct{}),
	}
}

func (h *revisionHub) currentRevision() RevisionTuple {
	h.mu.Lock()
	defer h.mu.Unlock()
	return h.current
}

func (h *revisionHub) publish(incoming RevisionTuple) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return
	}
	next := RevisionTuple{
		DataRevision:   max(h.current.DataRevision, incoming.DataRevision),
		StatusRevision: max(h.current.StatusRevision, incoming.StatusRevision),
	}
	if next == h.current {
		return
	}
	h.current = next
	for _, ch := range h.subscribers {
		select {
		case ch <- next:
		default:
			select {
			case <-ch:
			default:
			}
			select {
			case ch <- next:
			default:
			}
		}
	}
}

func (h *revisionHub) subscribe(ctx context.Context) <-chan RevisionTuple {
	ch := make(chan RevisionTuple, 1)
	h.mu.Lock()
	h.nextID++
	id := h.nextID
	h.subscribers[id] = ch
	ch <- h.current
	if h.closed {
		delete(h.subscribers, id)
		close(ch)
		h.mu.Unlock()
		return ch
	}
	done := h.done
	h.mu.Unlock()

	go func() {
		select {
		case <-ctx.Done():
			h.removeSubscriber(id)
		case <-done:
		}
	}()
	return ch
}

func (h *revisionHub) removeSubscriber(id uint64) {
	h.mu.Lock()
	defer h.mu.Unlock()
	ch, ok := h.subscribers[id]
	if !ok {
		return
	}
	delete(h.subscribers, id)
	close(ch)
}

func (h *revisionHub) close() {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.closed {
		return
	}
	h.closed = true
	close(h.done)
	for id, ch := range h.subscribers {
		delete(h.subscribers, id)
		close(ch)
	}
}
