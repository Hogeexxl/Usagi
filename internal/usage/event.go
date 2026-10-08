package usage

import (
	"fmt"
	"strings"
	"unicode"
)

type EventKind string

const (
	EventKindNormal           EventKind = "normal"
	EventKindRecovered        EventKind = "recovered"
	EventKindTurnCompensation EventKind = "turn_compensation"
)

func (kind EventKind) Validate() error {
	switch kind {
	case EventKindNormal, EventKindRecovered, EventKindTurnCompensation:
		return nil
	default:
		return fmt.Errorf("invalid event_kind: unknown value %q", kind)
	}
}

type CanonicalUsageEventWrite struct {
	EventID               string
	Kind                  EventKind
	OccurredAtMS          int64
	ThreadID              string
	RootSessionID         string
	TurnKey               *string
	Model                 string
	ReasoningEffort       *string
	EstimatedCostNanosUSD *int64
	Usage                 NormalizedTokenUsage
	CreatedAtMS           int64
}

func (event CanonicalUsageEventWrite) Validate() error {
	if err := validateEventString(event.EventID, "event_id"); err != nil {
		return err
	}
	if err := event.Kind.Validate(); err != nil {
		return err
	}
	if event.OccurredAtMS < 0 {
		return fmt.Errorf("invalid occurred_at_ms: must be non-negative")
	}
	if event.CreatedAtMS < 0 {
		return fmt.Errorf("invalid created_at_ms: must be non-negative")
	}
	if err := validateEventString(event.ThreadID, "thread_id"); err != nil {
		return err
	}
	if err := validateEventString(event.RootSessionID, "root_session_id"); err != nil {
		return err
	}
	if err := validateEventString(event.Model, "model"); err != nil {
		return err
	}
	if event.TurnKey != nil {
		if err := validateEventString(*event.TurnKey, "turn_key"); err != nil {
			return err
		}
	}
	if event.ReasoningEffort != nil {
		if err := validateEventString(*event.ReasoningEffort, "reasoning_effort"); err != nil {
			return err
		}
	}
	if event.EstimatedCostNanosUSD != nil && *event.EstimatedCostNanosUSD < 0 {
		return fmt.Errorf("invalid estimated_cost_nanos_usd: must be non-negative")
	}
	return event.Usage.Validate()
}

func validateEventString(value, field string) error {
	if value == "" || strings.TrimSpace(value) == "" {
		return fmt.Errorf("invalid %s: must not be empty", field)
	}
	for _, r := range value {
		if unicode.IsControl(r) {
			return fmt.Errorf("invalid %s: must not contain control characters", field)
		}
	}
	return nil
}
