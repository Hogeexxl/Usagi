package api

import (
	"net/url"
	"os"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/query"
)

func TestResolveRangeNamedAndCustomRanges(t *testing.T) {
	now := time.Date(2026, 1, 1, 0, 15, 0, 0, time.UTC)
	tests := []struct {
		key      string
		from     string
		to       string
		wantFrom string
		wantTo   string
	}{
		{key: "today", wantFrom: "2026-01-01", wantTo: "2026-01-01"},
		{key: "yesterday", wantFrom: "2025-12-31", wantTo: "2025-12-31"},
		{key: "7d", wantFrom: "2025-12-26", wantTo: "2026-01-01"},
		{key: "30d", wantFrom: "2025-12-03", wantTo: "2026-01-01"},
		{key: "year", wantFrom: "2026-01-01", wantTo: "2026-12-31"},
		{key: "custom", from: "2025-12-30", to: "2026-01-02", wantFrom: "2025-12-30", wantTo: "2026-01-02"},
	}
	for _, test := range tests {
		t.Run(test.key, func(t *testing.T) {
			values := url.Values{"range": {test.key}}
			if test.key == "custom" {
				values.Set("from", test.from)
				values.Set("to", test.to)
			}
			got, err := ResolveRangeAt(values, now, "UTC")
			if err != nil {
				t.Fatalf("ResolveRangeAt() error = %v", err)
			}
			if got.Key != test.key || got.Query != (query.DateRange{From: test.wantFrom, To: test.wantTo}) || got.Timezone != "UTC" {
				t.Fatalf("ResolveRangeAt() = %+v", got)
			}
			start, err := time.ParseInLocation(time.DateOnly, test.wantFrom, time.UTC)
			if err != nil {
				t.Fatal(err)
			}
			end, err := time.ParseInLocation(time.DateOnly, test.wantTo, time.UTC)
			if err != nil {
				t.Fatal(err)
			}
			if got.StartMS != start.UnixMilli() || got.EndMS != end.AddDate(0, 0, 1).UnixMilli() {
				t.Fatalf("boundaries = %d..%d, want %d..%d", got.StartMS, got.EndMS, start.UnixMilli(), end.AddDate(0, 0, 1).UnixMilli())
			}
		})
	}
}

func TestResolveRangeUsesLocalCivilDaysAcrossDST(t *testing.T) {
	now := time.Date(2026, 3, 8, 16, 0, 0, 0, time.UTC)
	got, err := ResolveRangeAt(url.Values{"range": {"today"}}, now, "America/New_York")
	if err != nil {
		t.Fatalf("ResolveRangeAt() error = %v", err)
	}
	if got.Query != (query.DateRange{From: "2026-03-08", To: "2026-03-08"}) || got.EndMS-got.StartMS != int64(23*time.Hour/time.Millisecond) {
		t.Fatalf("DST range = %+v, duration %v", got, time.Duration(got.EndMS-got.StartMS)*time.Millisecond)
	}
	if got.StartMS != time.Date(2026, 3, 8, 5, 0, 0, 0, time.UTC).UnixMilli() || got.EndMS != time.Date(2026, 3, 9, 4, 0, 0, 0, time.UTC).UnixMilli() {
		t.Fatalf("DST UTC boundaries = %d..%d", got.StartMS, got.EndMS)
	}
}

func TestResolveRangeMatchesRustMidnightOverlapAndHalfHourDST(t *testing.T) {
	havana, err := ResolveRangeAt(url.Values{"range": {"today"}}, time.Date(2026, 11, 1, 12, 0, 0, 0, time.UTC), "America/Havana")
	if err != nil {
		t.Fatalf("Havana overlap range error = %v", err)
	}
	if havana.StartMS != time.Date(2026, 11, 1, 4, 0, 0, 0, time.UTC).UnixMilli() ||
		havana.EndMS != time.Date(2026, 11, 2, 5, 0, 0, 0, time.UTC).UnixMilli() {
		t.Fatalf("Havana overlap boundaries = %s..%s", time.UnixMilli(havana.StartMS).UTC(), time.UnixMilli(havana.EndMS).UTC())
	}

	lordHowe, err := ResolveRangeAt(url.Values{"range": {"today"}}, time.Date(2026, 10, 4, 12, 0, 0, 0, time.UTC), "Australia/Lord_Howe")
	if err != nil {
		t.Fatalf("Lord Howe range error = %v", err)
	}
	if lordHowe.StartMS != time.Date(2026, 10, 3, 13, 30, 0, 0, time.UTC).UnixMilli() ||
		lordHowe.EndMS != time.Date(2026, 10, 4, 13, 0, 0, 0, time.UTC).UnixMilli() {
		t.Fatalf("Lord Howe boundaries = %s..%s", time.UnixMilli(lordHowe.StartMS).UTC(), time.UnixMilli(lordHowe.EndMS).UTC())
	}
}

func TestLocalTimezoneUsesIANANameFromEnvironment(t *testing.T) {
	t.Setenv("TZ", "America/New_York")
	name, location, err := LocalTimezone()
	if err != nil || name != "America/New_York" || location.String() != name {
		t.Fatalf("LocalTimezone() = %q, %v, %v", name, location, err)
	}
	t.Setenv("TZ", "../escape")
	if _, _, err := LocalTimezone(); err != ErrLocalTimeUnavailable {
		t.Fatalf("invalid TZ error = %v, want %v", err, ErrLocalTimeUnavailable)
	}
}

func TestLocalTimezoneUsesOSDefaultWhenTZIsUnset(t *testing.T) {
	previous, existed := os.LookupEnv("TZ")
	if err := os.Unsetenv("TZ"); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		var err error
		if existed {
			err = os.Setenv("TZ", previous)
		} else {
			err = os.Unsetenv("TZ")
		}
		if err != nil {
			t.Errorf("restore TZ: %v", err)
		}
	})

	name, location, err := LocalTimezone()
	if err != nil {
		t.Fatalf("LocalTimezone() without TZ error = %v", err)
	}
	if !validTimezoneName(name) || location.String() != name {
		t.Fatalf("LocalTimezone() without TZ = %q, %v", name, location)
	}
}

func TestResolveRangeHandlesMidnightGapAndSkippedDate(t *testing.T) {
	gap, err := ResolveRangeAt(url.Values{"range": {"today"}}, time.Date(2018, 11, 4, 12, 0, 0, 0, time.UTC), "America/Sao_Paulo")
	if err != nil {
		t.Fatalf("midnight gap range error = %v", err)
	}
	if gap.StartMS != time.Date(2018, 11, 4, 3, 0, 0, 0, time.UTC).UnixMilli() {
		t.Fatalf("midnight gap start = %s", time.UnixMilli(gap.StartMS).UTC())
	}
	skipped, err := ResolveRangeAt(url.Values{"range": {"custom"}, "from": {"2011-12-30"}, "to": {"2011-12-30"}}, time.Time{}, "Pacific/Apia")
	if err != nil {
		t.Fatalf("skipped date range error = %v", err)
	}
	if skipped.Query != (query.DateRange{From: "2011-12-30", To: "2011-12-30"}) || skipped.StartMS != skipped.EndMS {
		t.Fatalf("skipped date range = %+v", skipped)
	}
}

func TestResolveRangeRejectsWireOutsideContract(t *testing.T) {
	invalid := []url.Values{
		{},
		{"range": {""}},
		{"range": {"today", "year"}},
		{"range": {"today"}, "from": {"2026-01-01"}},
		{"range": {"today"}, "to": {"2026-01-01"}},
		{"range": {"custom"}},
		{"range": {"custom"}, "from": {"2026-01-01"}},
		{"range": {"custom"}, "from": {"2026-01-02"}, "to": {"2026-01-01"}},
		{"range": {"custom"}, "from": {"2026-02-30"}, "to": {"2026-03-01"}},
		{"range": {"week"}},
	}
	for _, values := range invalid {
		if _, err := ResolveRangeAt(values, time.Now(), "UTC"); err != query.ErrInvalidRange {
			t.Fatalf("ResolveRangeAt(%v) error = %v, want %v", values, err, query.ErrInvalidRange)
		}
	}
}
