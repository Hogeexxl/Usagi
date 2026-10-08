package usage

import "testing"

func TestEventKindValidation(t *testing.T) {
	for _, kind := range []EventKind{EventKindNormal, EventKindRecovered, EventKindTurnCompensation} {
		if err := kind.Validate(); err != nil {
			t.Errorf("EventKind(%q).Validate(): %v", kind, err)
		}
	}
	if err := EventKind("unknown").Validate(); err == nil {
		t.Fatal("EventKind accepted an unknown value")
	}
}

func TestUsageInputValidation(t *testing.T) {
	valid := validUsageEvent(t)
	if err := valid.Validate(); err != nil {
		t.Fatalf("valid event rejected: %v", err)
	}

	invalid := []struct {
		name   string
		change func(*CanonicalUsageEventWrite)
	}{
		{"empty event id", func(e *CanonicalUsageEventWrite) { e.EventID = "" }},
		{"blank event id", func(e *CanonicalUsageEventWrite) { e.EventID = "\u2003" }},
		{"control event id", func(e *CanonicalUsageEventWrite) { e.EventID = "event\x00id" }},
		{"unknown kind", func(e *CanonicalUsageEventWrite) { e.Kind = EventKind("normalised") }},
		{"negative occurrence time", func(e *CanonicalUsageEventWrite) { e.OccurredAtMS = -1 }},
		{"negative creation time", func(e *CanonicalUsageEventWrite) { e.CreatedAtMS = -1 }},
		{"blank thread id", func(e *CanonicalUsageEventWrite) { e.ThreadID = " \t" }},
		{"control root id", func(e *CanonicalUsageEventWrite) { e.RootSessionID = "root\u0085id" }},
		{"empty model", func(e *CanonicalUsageEventWrite) { e.Model = "" }},
		{"blank turn key", func(e *CanonicalUsageEventWrite) { e.TurnKey = stringPointer("\u2003") }},
		{"control reasoning effort", func(e *CanonicalUsageEventWrite) { e.ReasoningEffort = stringPointer("high\x7f") }},
		{"negative cost", func(e *CanonicalUsageEventWrite) { e.EstimatedCostNanosUSD = int64Pointer(-1) }},
		{"invalid tokens", func(e *CanonicalUsageEventWrite) { e.Usage = NormalizedTokenUsage{InputTokens: -1} }},
	}
	for _, test := range invalid {
		t.Run(test.name, func(t *testing.T) {
			event := valid
			test.change(&event)
			if err := event.Validate(); err == nil {
				t.Fatal("Validate succeeded")
			}
		})
	}
}

func validUsageEvent(t *testing.T) CanonicalUsageEventWrite {
	t.Helper()
	return CanonicalUsageEventWrite{
		EventID:               "event-1",
		Kind:                  EventKindNormal,
		OccurredAtMS:          100,
		ThreadID:              "thread-1",
		RootSessionID:         "root-1",
		TurnKey:               stringPointer("turn-1"),
		Model:                 "model-1",
		ReasoningEffort:       stringPointer("high"),
		EstimatedCostNanosUSD: int64Pointer(0),
		Usage:                 validTokenUsage(t, 10, 2, int64Pointer(1), 4, 1, 14),
		CreatedAtMS:           101,
	}
}

func stringPointer(value string) *string {
	return &value
}
