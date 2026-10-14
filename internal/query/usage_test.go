package query

import (
	"context"
	"errors"
	"testing"
	"time"
)

func TestSessionDatasetValidation(t *testing.T) {
	r := DateRange{From: "2026-10-11", To: "2026-10-12"}
	tests := []struct {
		name string
		q    SessionQuery
		want error
	}{
		{name: "invalid from", q: SessionQuery{Range: DateRange{From: "2026-02-30", To: r.To}}, want: ErrInvalidRange},
		{name: "invalid to", q: SessionQuery{Range: DateRange{From: r.From, To: "2026-10-11T00:00:00Z"}}, want: ErrInvalidRange},
		{name: "reversed range", q: SessionQuery{Range: DateRange{From: r.To, To: r.From}}, want: ErrInvalidRange},
		{name: "unknown project kind", q: SessionQuery{Range: r, Filter: UsageFilter{Projects: []ProjectSelection{{Kind: "other"}}}}, want: ErrInvalidFilter},
		{name: "missing project path", q: SessionQuery{Range: r, Filter: UsageFilter{Projects: []ProjectSelection{{Kind: ProjectKindProject}}}}, want: ErrInvalidFilter},
		{name: "projectless path", q: SessionQuery{Range: r, Filter: UsageFilter{Projects: []ProjectSelection{{Kind: ProjectKindProjectless, Path: "/a"}}}}, want: ErrInvalidFilter},
		{name: "unknown path", q: SessionQuery{Range: r, Filter: UsageFilter{Projects: []ProjectSelection{{Kind: ProjectKindUnknown, Path: "/a"}}}}, want: ErrInvalidFilter},
		{name: "workspace path", q: SessionQuery{Range: r, Filter: UsageFilter{Projects: []ProjectSelection{{Kind: ProjectKindWorkspace, Path: "/a"}}}}, want: ErrInvalidFilter},
	}
	service := newService(queryTestDB(t), nil, time.UTC)
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			if _, err := service.SessionDataset(context.Background(), test.q); !errors.Is(err, test.want) {
				t.Fatalf("SessionDataset() error = %v, want %v", err, test.want)
			}
		})
	}
}

func TestDecodeOfFollowsMagpieRules(t *testing.T) {
	base := DecodeInput{Output: 500, Millis: 10_000, TTFTMs: 2_000}
	tests := []struct {
		name   string
		mutate func(*DecodeInput)
		want   DecodeSample
		ok     bool
	}{
		{"plain reply counts from its first token", func(*DecodeInput) {}, DecodeSample{Tokens: 500, Ms: 8_000}, true},
		{"failed reply tells no speed", func(in *DecodeInput) { in.Failed = true }, DecodeSample{}, false},
		{"untimed reply tells no speed", func(in *DecodeInput) { in.TTFTMs = 0 }, DecodeSample{}, false},
		{"empty output tells no speed", func(in *DecodeInput) { in.Output = 0 }, DecodeSample{}, false},
		{"window under 100 ms tells no speed", func(in *DecodeInput) { in.Millis = 2_050 }, DecodeSample{}, false},
		{"over 10000 tokens a second tells no speed", func(in *DecodeInput) { in.Output = 100_000 }, DecodeSample{}, false},
		{"burst held back to the end tells no speed", func(in *DecodeInput) { in.FlowMs = 1_000 }, DecodeSample{}, false},
		{"flow close to the window still counts", func(in *DecodeInput) { in.FlowMs = 6_000 }, DecodeSample{Tokens: 500, Ms: 8_000}, true},
		{"reasoning reply counts its answer from the first text", func(in *DecodeInput) {
			in.Reasoning, in.FirstTextMs = 200, 4_000
		}, DecodeSample{Tokens: 300, Ms: 6_000}, true},
		{"reasoning reply without a first text tells no speed", func(in *DecodeInput) { in.Reasoning = 200 }, DecodeSample{}, false},
		{"answer far faster than its reasoning tells no speed", func(in *DecodeInput) {
			in.Reasoning, in.FirstTextMs = 10, 9_000
			in.Output, in.Millis = 5_010, 9_500
		}, DecodeSample{}, false},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			in := base
			test.mutate(&in)
			got, ok := DecodeOf(in)
			if ok != test.ok || got != test.want {
				t.Fatalf("DecodeOf(%+v) = %+v, %v; want %+v, %v", in, got, ok, test.want, test.ok)
			}
		})
	}
}

func TestSessionDatasetTotalsCountRetriedAndSpeed(t *testing.T) {
	db := queryTestDB(t)
	day := "2026-10-11"
	base := time.Date(2026, 10, 11, 9, 0, 0, 0, time.UTC)
	rows := []struct {
		at                    time.Time
		status, tries, output int
		millis, ttft          int64
		error                 string
		provider              string
		rejected              bool
	}{
		{at: base, status: 200, output: 500, millis: 10_000, ttft: 2_000},
		{at: base.Add(time.Second), status: 200, tries: 2, output: 100, millis: 3_000, ttft: 1_000},
		{at: base.Add(2 * time.Second), status: 502, tries: 3, output: 100, millis: 3_000, ttft: 1_000},
		{at: base.Add(3 * time.Second), status: 429, error: "rate"},
		{at: base.Add(4 * time.Second), status: 400, output: 999, rejected: true},
	}
	for _, row := range rows {
		provider := row.provider
		if provider == "" && !row.rejected {
			provider = "p"
		}
		if _, err := db.Exec(`INSERT INTO requests (t_ns,local_day,agent,provider,model,session,status,error,tries,output,ms,ttft_ms)
			VALUES (?,?,?,?,?,?,?,?,?,?,?,?)`, row.at.UnixNano(), day, "codex", provider, "m", "s", row.status,
			row.error, row.tries, row.output, row.millis, row.ttft); err != nil {
			t.Fatalf("insert speed request: %v", err)
		}
		if row.rejected {
			if _, err := db.Exec("UPDATE requests SET rejected = 1 WHERE t_ns = ?", row.at.UnixNano()); err != nil {
				t.Fatalf("mark rejected request: %v", err)
			}
		}
	}

	dataset, err := newService(db, nil, time.UTC).SessionDataset(context.Background(), SessionQuery{Range: DateRange{From: day, To: day}})
	if err != nil {
		t.Fatalf("SessionDataset() error = %v", err)
	}
	if len(dataset.Sessions) != 1 {
		t.Fatalf("SessionDataset sessions = %d, want 1", len(dataset.Sessions))
	}
	got := dataset.Sessions[0].Totals
	if got.Calls != 4 || got.Errors != 2 {
		t.Fatalf("calls/errors = %d/%d, want 4/2", got.Calls, got.Errors)
	}
	if got.Retried != 1 {
		t.Fatalf("Retried = %d, want 1: a failed retry does not count as answered on a retry", got.Retried)
	}
	if got.DecodeN != 2 || got.DecodeOut != 600 || got.DecodeMs != 10_000 {
		t.Fatalf("decode = n %d, out %d, ms %d; want 2, 600, 10000", got.DecodeN, got.DecodeOut, got.DecodeMs)
	}
}
