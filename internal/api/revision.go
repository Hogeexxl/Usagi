package api

import "sync"

type RevisionTuple struct {
	DataRevision   int64 `json:"data_revision"`
	StatusRevision int64 `json:"status_revision"`
}

type RevisionService interface {
	Current() RevisionTuple
	SetDataRevision(int64) RevisionTuple
	BumpStatus() RevisionTuple
	Subscribe() (<-chan RevisionTuple, func())
}

type revisionService struct {
	mu          sync.Mutex
	current     RevisionTuple
	nextID      uint64
	subscribers map[uint64]chan RevisionTuple
}

func NewRevisionService(dataRevision int64) RevisionService {
	return &revisionService{current: RevisionTuple{DataRevision: dataRevision}, subscribers: make(map[uint64]chan RevisionTuple)}
}

func (s *revisionService) Current() RevisionTuple {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.current
}

func (s *revisionService) SetDataRevision(value int64) RevisionTuple {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.current.DataRevision != value {
		s.current.DataRevision = value
		s.publishLocked()
	}
	return s.current
}

func (s *revisionService) BumpStatus() RevisionTuple {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.current.StatusRevision++
	s.publishLocked()
	return s.current
}

func (s *revisionService) Subscribe() (<-chan RevisionTuple, func()) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.subscribers == nil {
		s.subscribers = make(map[uint64]chan RevisionTuple)
	}
	s.nextID++
	id := s.nextID
	ch := make(chan RevisionTuple, 1)
	s.subscribers[id] = ch
	var once sync.Once
	return ch, func() {
		once.Do(func() {
			s.mu.Lock()
			delete(s.subscribers, id)
			close(ch)
			s.mu.Unlock()
		})
	}
}

func (s *revisionService) publishLocked() {
	for _, ch := range s.subscribers {
		select {
		case ch <- s.current:
		default:
			select {
			case <-ch:
			default:
			}
			ch <- s.current
		}
	}
}
