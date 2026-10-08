package storage

import (
	"context"
	"errors"
	"math"
	"sync"
	"testing"
)

func TestRevisionPerTransactionCap(t *testing.T) {
	db := testCurrent(t)
	ctx := context.Background()
	initial := db.CurrentRevision()
	var dataFirst, dataSecond, statusFirst, statusSecond int64
	err := db.WriteTx(ctx, func(tx *Tx) error {
		var err error
		dataFirst, err = tx.BumpDataRevision(ctx)
		if err != nil {
			return err
		}
		dataSecond, err = tx.BumpDataRevision(ctx)
		if err != nil {
			return err
		}
		statusFirst, err = tx.BumpStatusRevision(ctx)
		if err != nil {
			return err
		}
		statusSecond, err = tx.BumpStatusRevision(ctx)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	if dataFirst != initial.DataRevision+1 || dataSecond != dataFirst {
		t.Fatalf("data revisions = (%d, %d), initial %d", dataFirst, dataSecond, initial.DataRevision)
	}
	if statusFirst != initial.StatusRevision+1 || statusSecond != statusFirst {
		t.Fatalf("status revisions = (%d, %d), initial %d", statusFirst, statusSecond, initial.StatusRevision)
	}
	want := RevisionTuple{DataRevision: dataFirst, StatusRevision: statusFirst}
	if got := db.CurrentRevision(); got != want {
		t.Fatalf("CurrentRevision() = %+v, want %+v", got, want)
	}
}

func TestRevisionOverflowRollsBack(t *testing.T) {
	for _, test := range []struct {
		name   string
		data   int64
		status int64
		bump   func(context.Context, *Tx) error
	}{
		{"data", math.MaxInt64, 7, func(ctx context.Context, tx *Tx) error { _, err := tx.BumpDataRevision(ctx); return err }},
		{"status", 7, math.MaxInt64, func(ctx context.Context, tx *Tx) error { _, err := tx.BumpStatusRevision(ctx); return err }},
	} {
		t.Run(test.name, func(t *testing.T) {
			db := testCurrent(t)
			ctx := context.Background()
			if _, err := db.writer.ExecContext(ctx,
				"UPDATE app_meta SET data_revision=?,status_revision=? WHERE id=1",
				test.data, test.status,
			); err != nil {
				t.Fatal(err)
			}
			initialHub := db.CurrentRevision()
			err := db.WriteTx(ctx, func(tx *Tx) error {
				if _, err := tx.tx.ExecContext(ctx, "UPDATE app_meta SET cost_algorithm_version=42 WHERE id=1"); err != nil {
					return err
				}
				return test.bump(ctx, tx)
			})
			assertErrorKind(t, err, ErrorInvalidState)
			var got RevisionTuple
			var costVersion int64
			if err := db.readers.QueryRowContext(ctx,
				"SELECT data_revision,status_revision,cost_algorithm_version FROM app_meta WHERE id=1",
			).Scan(&got.DataRevision, &got.StatusRevision, &costVersion); err != nil {
				t.Fatal(err)
			}
			want := RevisionTuple{DataRevision: test.data, StatusRevision: test.status}
			if got != want || costVersion != 0 {
				t.Fatalf("stored revisions/cost version = (%+v, %d), want (%+v, 0)", got, costVersion, want)
			}
			if got := db.CurrentRevision(); got != initialHub {
				t.Fatalf("overflow changed hub revision to %+v, want %+v", got, initialHub)
			}
		})
	}
}

func TestRevisionCAS(t *testing.T) {
	db := testCurrent(t)
	ctx := context.Background()
	if _, err := db.writer.ExecContext(ctx, `
		CREATE TRIGGER ignore_data_revision_update
		BEFORE UPDATE OF data_revision ON app_meta
		BEGIN
			SELECT RAISE(IGNORE);
		END
	`); err != nil {
		t.Fatal(err)
	}
	initial := db.CurrentRevision()
	err := db.WriteTx(ctx, func(tx *Tx) error {
		_, err := tx.BumpDataRevision(ctx)
		return err
	})
	assertErrorKind(t, err, ErrorInvalidState)
	if got := db.CurrentRevision(); got != initial {
		t.Fatalf("failed CAS changed hub to %+v, want %+v", got, initial)
	}
	var stored int64
	if err := db.readers.QueryRowContext(ctx, "SELECT data_revision FROM app_meta WHERE id=1").Scan(&stored); err != nil {
		t.Fatal(err)
	}
	if stored != initial.DataRevision {
		t.Fatalf("failed CAS stored data revision %d, want %d", stored, initial.DataRevision)
	}
}

func TestRevisionPublishesOnlyAfterCommit(t *testing.T) {
	db := testCurrent(t)
	ctx := context.Background()
	initial := db.CurrentRevision()
	subscription := db.SubscribeRevisions(ctx)
	if got := <-subscription; got != initial {
		t.Fatalf("initial subscription revision = %+v, want %+v", got, initial)
	}

	callbackErr := errors.New("rollback requested")
	err := db.WriteTx(ctx, func(tx *Tx) error {
		if _, err := tx.BumpStatusRevision(ctx); err != nil {
			return err
		}
		return callbackErr
	})
	if err != callbackErr {
		t.Fatalf("callback rollback error = %v, want %v", err, callbackErr)
	}
	assertNoRevisionNotification(t, subscription)

	panicValue := &struct{ message string }{"revision panic"}
	func() {
		defer func() {
			if got := recover(); got != panicValue {
				t.Fatalf("panic value = %v, want identical value %v", got, panicValue)
			}
		}()
		_ = db.WriteTx(ctx, func(tx *Tx) error {
			if _, err := tx.BumpStatusRevision(ctx); err != nil {
				return err
			}
			panic(panicValue)
		})
	}()
	assertNoRevisionNotification(t, subscription)

	commitErr := db.WriteTx(ctx, func(tx *Tx) error {
		if _, err := tx.BumpStatusRevision(ctx); err != nil {
			return err
		}
		if _, err := tx.tx.ExecContext(ctx, "CREATE TABLE revision_commit_parent (id INTEGER PRIMARY KEY)"); err != nil {
			return err
		}
		if _, err := tx.tx.ExecContext(ctx, `
			CREATE TABLE revision_commit_child (
				id INTEGER PRIMARY KEY,
				parent_id INTEGER,
				FOREIGN KEY(parent_id) REFERENCES revision_commit_parent(id)
					DEFERRABLE INITIALLY DEFERRED
			)
		`); err != nil {
			return err
		}
		_, err := tx.tx.ExecContext(ctx, "INSERT INTO revision_commit_child(id,parent_id) VALUES(1,99)")
		return err
	})
	if commitErr == nil {
		t.Fatal("WriteTx succeeded with a deferred foreign-key violation")
	}
	assertNoRevisionNotification(t, subscription)
	if got := db.CurrentRevision(); got != initial {
		t.Fatalf("failed Commit changed hub to %+v, want %+v", got, initial)
	}

	if err := db.WriteTx(ctx, func(tx *Tx) error {
		if _, err := tx.BumpStatusRevision(ctx); err != nil {
			return err
		}
		if got := db.CurrentRevision(); got != initial {
			return errors.New("revision hub changed before callback returned")
		}
		select {
		case <-subscription:
			return errors.New("subscriber received a revision before callback returned")
		default:
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	want := RevisionTuple{DataRevision: initial.DataRevision, StatusRevision: initial.StatusRevision + 1}
	if got := db.CurrentRevision(); got != want {
		t.Fatalf("committed revision = %+v, want %+v", got, want)
	}
	if got := <-subscription; got != want {
		t.Fatalf("published revision = %+v, want %+v", got, want)
	}
}

func TestRevisionHubLatestWins(t *testing.T) {
	initial := RevisionTuple{DataRevision: 10, StatusRevision: 4}
	hub := newRevisionHub(initial)
	subscription := hub.subscribe(context.Background())
	if got := <-subscription; got != initial {
		t.Fatalf("initial revision = %+v, want %+v", got, initial)
	}
	for _, incoming := range []RevisionTuple{
		{DataRevision: 8, StatusRevision: 10},
		{DataRevision: 12, StatusRevision: 2},
		{DataRevision: 11, StatusRevision: 8},
	} {
		hub.publish(incoming)
	}
	want := RevisionTuple{DataRevision: 12, StatusRevision: 10}
	if got := <-subscription; got != want {
		t.Fatalf("latest revision = %+v, want %+v", got, want)
	}
	hub.publish(RevisionTuple{DataRevision: 1, StatusRevision: 1})
	if got := hub.currentRevision(); got != want {
		t.Fatalf("hub moved backward to %+v, want %+v", got, want)
	}
	assertNoRevisionNotification(t, subscription)
	hub.close()
}

func TestRevisionHubCloseClosesSubscribers(t *testing.T) {
	db := testCurrent(t)
	ctx := context.Background()
	first := db.SubscribeRevisions(ctx)
	second := db.SubscribeRevisions(ctx)
	initial := db.CurrentRevision()
	if got := <-first; got != initial {
		t.Fatalf("first initial revision = %+v, want %+v", got, initial)
	}
	if got := <-second; got != initial {
		t.Fatalf("second initial revision = %+v, want %+v", got, initial)
	}
	if err := db.Close(); err != nil {
		t.Fatal(err)
	}
	assertRevisionChannelClosed(t, first)
	assertRevisionChannelClosed(t, second)

	closedSubscription := db.SubscribeRevisions(ctx)
	if got := <-closedSubscription; got != initial {
		t.Fatalf("post-close initial revision = %+v, want %+v", got, initial)
	}
	assertRevisionChannelClosed(t, closedSubscription)
}

func TestRevisionHubCancelCloseRace(t *testing.T) {
	for range 128 {
		hub := newRevisionHub(RevisionTuple{})
		ctx, cancel := context.WithCancel(context.Background())
		subscription := hub.subscribe(ctx)
		<-subscription
		start := make(chan struct{})
		var wait sync.WaitGroup
		wait.Add(2)
		go func() {
			defer wait.Done()
			<-start
			cancel()
		}()
		go func() {
			defer wait.Done()
			<-start
			hub.close()
		}()
		close(start)
		wait.Wait()
		assertRevisionChannelClosed(t, subscription)
	}
}

func assertNoRevisionNotification(t *testing.T, ch <-chan RevisionTuple) {
	t.Helper()
	select {
	case got, ok := <-ch:
		t.Fatalf("unexpected revision notification (%+v, open=%t)", got, ok)
	default:
	}
}

func assertRevisionChannelClosed(t *testing.T, ch <-chan RevisionTuple) {
	t.Helper()
	if got, ok := <-ch; ok {
		t.Fatalf("revision channel returned unexpected value %+v before closing", got)
	}
}
