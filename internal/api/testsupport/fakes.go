package testsupport

import (
	"context"
	"sync"
	"time"

	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/projection"
)

type ProjectionService interface {
	Summary(context.Context, projection.ScopeQuery) (projection.SummaryView, error)
	ModelDistribution(context.Context, projection.ScopeQuery) (projection.ModelDistributionView, error)
	ProjectDistribution(context.Context, projection.ScopeQuery) (projection.ProjectDistributionView, error)
	FilterOptions(context.Context, projection.ScopeQuery) (projection.FilterOptionsView, error)
	Sessions(context.Context, projection.ScopeQuery) (projection.SessionListView, error)
	SessionDetail(context.Context, projection.ScopeQuery, projection.ThreadKey) (projection.SessionDetailView, error)
	Quotas(context.Context, bool) (projection.QuotaView, error)
}

type FakeProjectionService struct {
	SummaryResult             projection.SummaryView
	SummaryErr                error
	ModelDistributionResult   projection.ModelDistributionView
	ModelDistributionErr      error
	ProjectDistributionResult projection.ProjectDistributionView
	ProjectDistributionErr    error
	FilterOptionsResult       projection.FilterOptionsView
	FilterOptionsErr          error
	SessionsResult            projection.SessionListView
	SessionsErr               error
	SessionDetailResult       projection.SessionDetailView
	SessionDetailErr          error
	Quota                     FakeQuotaService

	mu                         sync.Mutex
	SummaryQueries             []projection.ScopeQuery
	ModelDistributionQueries   []projection.ScopeQuery
	ProjectDistributionQueries []projection.ScopeQuery
	SessionQueries             []projection.ScopeQuery
	SessionDetailQueries       []projection.ScopeQuery
	SessionDetailKeys          []projection.ThreadKey
}

func (f *FakeProjectionService) Summary(_ context.Context, q projection.ScopeQuery) (projection.SummaryView, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.SummaryQueries = append(f.SummaryQueries, q)
	return f.SummaryResult, f.SummaryErr
}

func (f *FakeProjectionService) ModelDistribution(_ context.Context, q projection.ScopeQuery) (projection.ModelDistributionView, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.ModelDistributionQueries = append(f.ModelDistributionQueries, q)
	return f.ModelDistributionResult, f.ModelDistributionErr
}

func (f *FakeProjectionService) ProjectDistribution(_ context.Context, q projection.ScopeQuery) (projection.ProjectDistributionView, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.ProjectDistributionQueries = append(f.ProjectDistributionQueries, q)
	return f.ProjectDistributionResult, f.ProjectDistributionErr
}

func (f *FakeProjectionService) FilterOptions(context.Context, projection.ScopeQuery) (projection.FilterOptionsView, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.FilterOptionsResult, f.FilterOptionsErr
}

func (f *FakeProjectionService) Sessions(_ context.Context, q projection.ScopeQuery) (projection.SessionListView, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.SessionQueries = append(f.SessionQueries, q)
	return f.SessionsResult, f.SessionsErr
}

func (f *FakeProjectionService) SessionDetail(_ context.Context, q projection.ScopeQuery, key projection.ThreadKey) (projection.SessionDetailView, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.SessionDetailQueries = append(f.SessionDetailQueries, q)
	f.SessionDetailKeys = append(f.SessionDetailKeys, key)
	return f.SessionDetailResult, f.SessionDetailErr
}

func (f *FakeProjectionService) Quotas(ctx context.Context, asked bool) (projection.QuotaView, error) {
	return f.Quota.Quotas(ctx, asked)
}

type FakeQuotaService struct {
	Result projection.QuotaView
	Err    error

	mu    sync.Mutex
	Asked []bool
}

type FakeWatcherQuery struct {
	mu                 sync.Mutex
	InitialSyncPending bool
	SyncRevision       int64
	SyncErr            error
	SyncCalls          int
}

func (f *FakeWatcherQuery) Sync(context.Context) (int64, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.SyncCalls++
	return f.SyncRevision, f.SyncErr
}

func (f *FakeWatcherQuery) InitialSyncComplete(context.Context) (bool, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	return !f.InitialSyncPending, nil
}

func (f *FakeWatcherQuery) SyncCallCount() int {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.SyncCalls
}

func (f *FakeQuotaService) Quotas(_ context.Context, asked bool) (projection.QuotaView, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.Asked = append(f.Asked, asked)
	return f.Result, f.Err
}

type RuntimeStatusSource interface {
	Status() magpie.RuntimeStatus
	Subscribe() (<-chan magpie.RuntimeStatus, func())
}

type FakeRuntime struct {
	mu          sync.Mutex
	status      magpie.RuntimeStatus
	nextID      uint64
	subscribers map[uint64]*fakeRuntimeSubscriber
}

func NewFakeRuntime(status magpie.RuntimeStatus) *FakeRuntime {
	return &FakeRuntime{status: status, subscribers: make(map[uint64]*fakeRuntimeSubscriber)}
}

func (f *FakeRuntime) Status() magpie.RuntimeStatus {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.status
}

func (f *FakeRuntime) SetStatus(status magpie.RuntimeStatus) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.status == status {
		return
	}
	f.status = status
	for _, subscriber := range f.subscribers {
		subscriber.publish(status)
	}
}

func (f *FakeRuntime) Subscribe() (<-chan magpie.RuntimeStatus, func()) {
	f.mu.Lock()
	f.nextID++
	id := f.nextID
	subscriber := newFakeRuntimeSubscriber()
	f.subscribers[id] = subscriber
	subscriber.publish(f.status)
	f.mu.Unlock()
	var once sync.Once
	return subscriber.out, func() {
		once.Do(func() {
			f.mu.Lock()
			delete(f.subscribers, id)
			f.mu.Unlock()
			subscriber.close()
		})
	}
}

type fakeRuntimeSubscriber struct {
	mu     sync.Mutex
	queue  []magpie.RuntimeStatus
	head   int
	closed bool
	wake   chan struct{}
	done   chan struct{}
	out    chan magpie.RuntimeStatus
}

func newFakeRuntimeSubscriber() *fakeRuntimeSubscriber {
	subscriber := &fakeRuntimeSubscriber{
		wake: make(chan struct{}, 1), done: make(chan struct{}), out: make(chan magpie.RuntimeStatus),
	}
	go subscriber.run()
	return subscriber
}

func (s *fakeRuntimeSubscriber) publish(status magpie.RuntimeStatus) {
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

func (s *fakeRuntimeSubscriber) close() {
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

func (s *fakeRuntimeSubscriber) run() {
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

type Clock interface {
	Now() time.Time
}

type FakeClock struct {
	mu  sync.Mutex
	now time.Time
}

func NewFakeClock(now time.Time) *FakeClock {
	return &FakeClock{now: now}
}

func (f *FakeClock) Now() time.Time {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.now
}

func (f *FakeClock) Advance(d time.Duration) time.Time {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.now = f.now.Add(d)
	return f.now
}

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

type FakeRevisionService struct {
	mu          sync.Mutex
	current     RevisionTuple
	nextID      uint64
	subscribers map[uint64]chan RevisionTuple
}

func NewFakeRevisionService(initial RevisionTuple) *FakeRevisionService {
	return &FakeRevisionService{current: initial, subscribers: make(map[uint64]chan RevisionTuple)}
}

func (f *FakeRevisionService) Current() RevisionTuple {
	f.mu.Lock()
	defer f.mu.Unlock()
	return f.current
}

func (f *FakeRevisionService) SetDataRevision(value int64) RevisionTuple {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.current.DataRevision != value {
		f.current.DataRevision = value
		f.publishLocked()
	}
	return f.current
}

func (f *FakeRevisionService) BumpStatus() RevisionTuple {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.current.StatusRevision++
	f.publishLocked()
	return f.current
}

func (f *FakeRevisionService) Subscribe() (<-chan RevisionTuple, func()) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.nextID++
	id := f.nextID
	ch := make(chan RevisionTuple, 1)
	f.subscribers[id] = ch
	var once sync.Once
	return ch, func() {
		once.Do(func() {
			f.mu.Lock()
			delete(f.subscribers, id)
			close(ch)
			f.mu.Unlock()
		})
	}
}

func (f *FakeRevisionService) publishLocked() {
	for _, ch := range f.subscribers {
		select {
		case ch <- f.current:
		default:
			select {
			case <-ch:
			default:
			}
			ch <- f.current
		}
	}
}
