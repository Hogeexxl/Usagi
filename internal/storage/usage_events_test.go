package storage

import (
	"context"
	"database/sql"
	"testing"

	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/usage"
)

func canonicalUsageEvent() usage.CanonicalUsageEventWrite {
	write := int64(0)
	return usage.CanonicalUsageEventWrite{
		EventID: "event", Kind: usage.EventKindNormal, OccurredAtMS: 10,
		ThreadID: "thread", RootSessionID: "root", Model: "model",
		Usage:       usage.NormalizedTokenUsage{InputTokens: 10, CachedTokens: 2, CacheWriteTokens: &write, OutputTokens: 4, ReasoningTokens: 1, TotalTokens: 14},
		CreatedAtMS: 11,
	}
}

func canonicalUsageDB(t *testing.T) *DB {
	t.Helper()
	db := openCanonicalTestDB(t)
	for _, name := range []string{"thread", "root"} {
		identity, patch := canonicalThreadPatch(t, name, domain.SourceCodex)
		canonicalUpsert(t, db, identity, patch)
	}
	return db
}

func canonicalWriteUsage(t *testing.T, db *DB, event usage.CanonicalUsageEventWrite) UsageWriteOutcome {
	t.Helper()
	var outcome UsageWriteOutcome
	err := db.WriteTx(context.Background(), func(tx *Tx) error {
		var err error
		outcome, err = tx.WriteUsageEvent(context.Background(), domain.SourceCodex, 1, event)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	return outcome
}

func TestUsageQualityDerivation(t *testing.T) {
	db := canonicalUsageDB(t)
	for _, test := range []struct {
		id, quality string
		cacheWrite  *int64
	}{
		{"unknown", "partial", nil}, {"zero", "complete", canonicalIntPointer(0)},
	} {
		event := canonicalUsageEvent()
		event.EventID = test.id
		event.Usage.CacheWriteTokens = test.cacheWrite
		if got := canonicalWriteUsage(t, db, event); got != UsageInserted {
			t.Fatalf("write = %d", got)
		}
		var quality string
		var stored sql.NullInt64
		err := db.readers.QueryRowContext(context.Background(), "SELECT quality_status,cache_write_tokens FROM usage_events WHERE source=? AND source_epoch=? AND event_id=?", domain.SourceCodex, 1, test.id).Scan(&quality, &stored)
		if err != nil {
			t.Fatal(err)
		}
		if quality != test.quality || stored.Valid != (test.cacheWrite != nil) || (stored.Valid && stored.Int64 != 0) {
			t.Fatalf("quality/null = %q/%+v", quality, stored)
		}
		if got := canonicalWriteUsage(t, db, event); got != UsageDuplicate {
			t.Fatalf("repeat = %d", got)
		}
	}
}

func TestUsageDuplicate(t *testing.T) {
	ctx := context.Background()
	db := canonicalUsageDB(t)
	event := canonicalUsageEvent()
	if got := canonicalWriteUsage(t, db, event); got != UsageInserted {
		t.Fatalf("first write = %d", got)
	}
	if got := canonicalWriteUsage(t, db, event); got != UsageDuplicate {
		t.Fatalf("second write = %d", got)
	}
	err := db.WriteTx(ctx, func(tx *Tx) error {
		match, err := tx.CompareUsageEvent(ctx, domain.SourceCodex, 1, event)
		if err != nil {
			return err
		}
		if match != UsageEventIdentical {
			t.Errorf("compare = %d", match)
		}
		match, err = tx.CompareUsageEvent(ctx, domain.SourceCodex, 2, event)
		if err != nil {
			return err
		}
		if match != UsageEventAbsent {
			t.Errorf("different epoch compare = %d", match)
		}
		match, err = tx.CompareUsageEvent(ctx, domain.SourceAntigravity, 1, event)
		if err != nil {
			return err
		}
		if match != UsageEventAbsent {
			t.Errorf("different source compare = %d", match)
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	var count int
	if err := db.readers.QueryRowContext(ctx, "SELECT count(*) FROM usage_events").Scan(&count); err != nil {
		t.Fatal(err)
	}
	if count != 1 {
		t.Fatalf("event count = %d", count)
	}
}

func TestUsageConflict(t *testing.T) {
	ctx := context.Background()
	db := canonicalUsageDB(t)
	original := canonicalUsageEvent()
	canonicalWriteUsage(t, db, original)
	for _, test := range []struct {
		name   string
		change func(*usage.CanonicalUsageEventWrite)
	}{
		{"kind", func(e *usage.CanonicalUsageEventWrite) { e.Kind = usage.EventKindRecovered }},
		{"occurrence time", func(e *usage.CanonicalUsageEventWrite) { e.OccurredAtMS++ }},
		{"thread", func(e *usage.CanonicalUsageEventWrite) { e.ThreadID = "other" }},
		{"root", func(e *usage.CanonicalUsageEventWrite) { e.RootSessionID = "other" }},
		{"turn key", func(e *usage.CanonicalUsageEventWrite) { e.TurnKey = canonicalStringPointer("turn") }},
		{"model", func(e *usage.CanonicalUsageEventWrite) { e.Model = "other" }},
		{"reasoning effort", func(e *usage.CanonicalUsageEventWrite) { e.ReasoningEffort = canonicalStringPointer("high") }},
		{"input and total", func(e *usage.CanonicalUsageEventWrite) { e.Usage.InputTokens++; e.Usage.TotalTokens++ }},
		{"cached", func(e *usage.CanonicalUsageEventWrite) { e.Usage.CachedTokens++ }},
		{"cache write", func(e *usage.CanonicalUsageEventWrite) { e.Usage.CacheWriteTokens = canonicalIntPointer(1) }},
		{"unknown cache write", func(e *usage.CanonicalUsageEventWrite) { e.Usage.CacheWriteTokens = nil }},
		{"output and total", func(e *usage.CanonicalUsageEventWrite) { e.Usage.OutputTokens++; e.Usage.TotalTokens++ }},
		{"reasoning", func(e *usage.CanonicalUsageEventWrite) { e.Usage.ReasoningTokens++ }},
	} {
		t.Run(test.name, func(t *testing.T) {
			event := original
			test.change(&event)
			err := db.WriteTx(ctx, func(tx *Tx) error {
				match, err := tx.CompareUsageEvent(ctx, domain.SourceCodex, 1, event)
				if err != nil {
					return err
				}
				if match != UsageEventConflict {
					t.Fatalf("compare = %d, want Conflict", match)
				}
				_, err = tx.WriteUsageEvent(ctx, domain.SourceCodex, 1, event)
				return err
			})
			requireCanonicalInvalidState(t, err)
		})
	}
	if got := canonicalWriteUsage(t, db, original); got != UsageDuplicate {
		t.Fatalf("original payload was changed: %d", got)
	}
}

func TestUsageDerivedFieldsExcluded(t *testing.T) {
	ctx := context.Background()
	db := canonicalUsageDB(t)
	event := canonicalUsageEvent()
	event.EstimatedCostNanosUSD = canonicalIntPointer(1)
	canonicalWriteUsage(t, db, event)
	event.EstimatedCostNanosUSD = canonicalIntPointer(99)
	event.CreatedAtMS = 100
	if got := canonicalWriteUsage(t, db, event); got != UsageDuplicate {
		t.Fatalf("derived change write = %d", got)
	}
	var cost, created int64
	err := db.readers.QueryRowContext(ctx, "SELECT estimated_cost_nanos_usd,created_at_ms FROM usage_events WHERE source=? AND source_epoch=? AND event_id=?", domain.SourceCodex, 1, event.EventID).Scan(&cost, &created)
	if err != nil {
		t.Fatal(err)
	}
	if cost != 1 || created != 11 {
		t.Fatalf("duplicate changed derived fields: %d, %d", cost, created)
	}
}

func TestUsageThreadSource(t *testing.T) {
	ctx := context.Background()
	db := canonicalUsageDB(t)
	foreignIdentity, foreignPatch := canonicalThreadPatch(t, "foreign", domain.SourceAntigravity)
	canonicalUpsert(t, db, foreignIdentity, foreignPatch)
	for _, test := range []struct{ thread, root string }{
		{"absent", "root"}, {"thread", "absent"}, {"foreign", "root"}, {"thread", "foreign"},
	} {
		event := canonicalUsageEvent()
		event.ThreadID, event.RootSessionID = test.thread, test.root
		err := db.WriteTx(ctx, func(tx *Tx) error {
			match, err := tx.CompareUsageEvent(ctx, domain.SourceCodex, 1, event)
			if err != nil {
				return err
			}
			if match != UsageEventAbsent {
				t.Fatalf("compare without thread gate = %d", match)
			}
			_, err = tx.WriteUsageEvent(ctx, domain.SourceCodex, 1, event)
			return err
		})
		requireCanonicalInvalidState(t, err)
	}
	for _, test := range []struct {
		source domain.SourceID
		epoch  int64
	}{
		{domain.SourceID("Bad"), 1}, {domain.SourceCodex, 0}, {domain.SourceCodex, -1},
	} {
		requireCanonicalInvalidState(t, db.WriteTx(ctx, func(tx *Tx) error {
			_, err := tx.WriteUsageEvent(ctx, test.source, test.epoch, canonicalUsageEvent())
			return err
		}))
	}
	invalid := canonicalUsageEvent()
	invalid.Model = "\u2003"
	requireCanonicalInvalidState(t, db.WriteTx(ctx, func(tx *Tx) error { _, err := tx.WriteUsageEvent(ctx, domain.SourceCodex, 1, invalid); return err }))
}

func canonicalIntPointer(value int64) *int64      { return &value }
func canonicalStringPointer(value string) *string { return &value }
