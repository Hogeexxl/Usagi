package app

import (
	"context"
	"sync"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

type MagpieSource interface {
	Status() magpie.RuntimeStatus
	Client() (*magpie.Client, bool)
	Subscribe() (<-chan magpie.RuntimeStatus, func())
}

type MagpieRuntime interface {
	MagpieSource
	Start(context.Context) error
	Close() error
}

type RuntimeFactory func() MagpieRuntime

type statusSubscriber struct {
	mu     sync.Mutex
	queue  []magpie.RuntimeStatus
	head   int
	closed bool
	wake   chan struct{}
	done   chan struct{}
	out    chan magpie.RuntimeStatus
}

func newStatusSubscriber(initial magpie.RuntimeStatus) *statusSubscriber {
	subscriber := &statusSubscriber{
		wake: make(chan struct{}, 1),
		done: make(chan struct{}),
		out:  make(chan magpie.RuntimeStatus),
	}
	subscriber.publish(initial)
	go subscriber.run()
	return subscriber
}

func (s *statusSubscriber) publish(status magpie.RuntimeStatus) {
	s.mu.Lock()
	if !s.closed {
		s.queue = append(s.queue, status)
	}
	s.mu.Unlock()
	select {
	case s.wake <- struct{}{}:
	default:
	}
}

func (s *statusSubscriber) close() {
	s.mu.Lock()
	if !s.closed {
		s.closed = true
		close(s.done)
	}
	s.mu.Unlock()
	select {
	case s.wake <- struct{}{}:
	default:
	}
}

func (s *statusSubscriber) run() {
	defer close(s.out)
	for {
		s.mu.Lock()
		if s.head < len(s.queue) {
			status := s.queue[s.head]
			s.head++
			if s.head == len(s.queue) {
				s.queue = nil
				s.head = 0
			}
			s.mu.Unlock()
			select {
			case s.out <- status:
			case <-s.done:
				return
			}
			continue
		}
		closed := s.closed
		s.mu.Unlock()
		if closed {
			return
		}
		select {
		case <-s.wake:
		case <-s.done:
			return
		}
	}
}

type MagpieSupervisor struct {
	factory       RuntimeFactory
	retryInterval time.Duration

	mu         sync.RWMutex
	status     magpie.RuntimeStatus
	current    MagpieRuntime
	subs       map[uint64]*statusSubscriber
	nextSubID  uint64
	runStarted bool
	closed     bool
	done       chan struct{}
	doneClosed bool
	cancel     context.CancelFunc
	closeErr   error
}

func NewMagpieSupervisor(factory RuntimeFactory, retryInterval time.Duration) *MagpieSupervisor {
	return &MagpieSupervisor{
		factory:       factory,
		retryInterval: retryInterval,
		status:        magpie.RuntimeStatusStarting,
		subs:          make(map[uint64]*statusSubscriber),
		done:          make(chan struct{}),
	}
}

func (s *MagpieSupervisor) Run(ctx context.Context) {
	s.mu.Lock()
	if s.closed || s.runStarted {
		s.mu.Unlock()
		return
	}
	s.runStarted = true
	runCtx, cancel := context.WithCancel(ctx)
	s.cancel = cancel
	s.mu.Unlock()

	var current MagpieRuntime
	var unsubscribe func()
	defer func() {
		cancel()
		if unsubscribe != nil {
			unsubscribe()
		}
		if current != nil {
			s.clearCurrent(current)
			s.recordCloseError(current.Close())
		}
		s.mu.Lock()
		s.cancel = nil
		s.finishLocked()
		s.mu.Unlock()
	}()

	for {
		if runCtx.Err() != nil {
			return
		}

		runtime := s.factory()
		startErr := runtime.Start(runCtx)
		if runCtx.Err() != nil {
			_ = runtime.Close()
			return
		}
		if startErr != nil {
			_ = runtime.Close()
			s.setState(nil, magpie.RuntimeStatusUnavailable)
			if !waitForRetry(runCtx, s.retryInterval) {
				return
			}
			continue
		}

		statuses, stopSubscription := runtime.Subscribe()
		if !s.setState(runtime, magpie.RuntimeStatusReady) {
			stopSubscription()
			_ = runtime.Close()
			return
		}
		current = runtime
		unsubscribe = stopSubscription

		recover := false
		for !recover {
			select {
			case <-runCtx.Done():
				return
			case status, ok := <-statuses:
				if !ok {
					statuses = nil
					continue
				}
				if status == magpie.RuntimeStatusUnavailable {
					s.setState(nil, magpie.RuntimeStatusUnavailable)
					unsubscribe()
					unsubscribe = nil
					s.recordCloseError(current.Close())
					current = nil
					recover = true
				}
			}
		}

		if !waitForRetry(runCtx, s.retryInterval) {
			return
		}
	}
}

func waitForRetry(ctx context.Context, interval time.Duration) bool {
	timer := time.NewTimer(interval)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-timer.C:
		return ctx.Err() == nil
	}
}

func (s *MagpieSupervisor) Status() magpie.RuntimeStatus {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.status
}

func (s *MagpieSupervisor) Client() (*magpie.Client, bool) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	if s.status != magpie.RuntimeStatusReady || s.current == nil {
		return nil, false
	}
	return s.current.Client()
}

func (s *MagpieSupervisor) Subscribe() (<-chan magpie.RuntimeStatus, func()) {
	s.mu.Lock()
	s.nextSubID++
	id := s.nextSubID
	subscriber := newStatusSubscriber(s.status)
	s.subs[id] = subscriber
	s.mu.Unlock()

	var once sync.Once
	return subscriber.out, func() {
		once.Do(func() {
			s.mu.Lock()
			delete(s.subs, id)
			s.mu.Unlock()
			subscriber.close()
		})
	}
}

func (s *MagpieSupervisor) Close() error {
	s.mu.Lock()
	s.closed = true
	cancel := s.cancel
	if !s.runStarted {
		s.finishLocked()
	}
	done := s.done
	s.mu.Unlock()

	if cancel != nil {
		cancel()
	}
	<-done

	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.closeErr
}

func (s *MagpieSupervisor) setState(runtime MagpieRuntime, status magpie.RuntimeStatus) bool {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return false
	}
	s.current = runtime
	if s.status != status {
		s.status = status
		for _, subscriber := range s.subs {
			subscriber.publish(status)
		}
	}
	s.mu.Unlock()
	return true
}

func (s *MagpieSupervisor) clearCurrent(runtime MagpieRuntime) {
	s.mu.Lock()
	if s.current == runtime {
		s.current = nil
	}
	s.mu.Unlock()
}

func (s *MagpieSupervisor) recordCloseError(err error) {
	if err == nil {
		return
	}
	s.mu.Lock()
	if s.closeErr == nil {
		s.closeErr = err
	}
	s.mu.Unlock()
}

func (s *MagpieSupervisor) finishLocked() {
	if !s.doneClosed {
		close(s.done)
		s.doneClosed = true
	}
}
