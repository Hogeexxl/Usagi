package domain

import "fmt"

type SourceUsageEpochState struct {
	Source              SourceID
	ActiveEpoch         int64
	BuildEpoch          *int64
	ActiveParserVersion int64
	BuildParserVersion  *int64
}

func NewSourceUsageEpochState(
	source SourceID,
	activeEpoch int64,
	buildEpoch *int64,
	activeParserVersion int64,
	buildParserVersion *int64,
) (SourceUsageEpochState, error) {
	value := SourceUsageEpochState{
		Source:              source,
		ActiveEpoch:         activeEpoch,
		BuildEpoch:          buildEpoch,
		ActiveParserVersion: activeParserVersion,
		BuildParserVersion:  buildParserVersion,
	}
	if err := value.Validate(); err != nil {
		return SourceUsageEpochState{}, err
	}
	return value, nil
}

func (s SourceUsageEpochState) Validate() error {
	if err := s.Source.Validate(); err != nil {
		return fmt.Errorf("source: %w", err)
	}
	if s.ActiveEpoch < 0 {
		return fmt.Errorf("invalid active_epoch: must be non-negative")
	}
	if s.ActiveParserVersion < 0 {
		return fmt.Errorf("invalid active_parser_version: must be non-negative")
	}
	if (s.BuildEpoch == nil) != (s.BuildParserVersion == nil) {
		return fmt.Errorf("build_epoch and build_parser_version must both be null or non-null")
	}
	if s.BuildEpoch == nil {
		return nil
	}
	if *s.BuildEpoch < 1 {
		return fmt.Errorf("invalid build_epoch: must be positive")
	}
	if *s.BuildParserVersion < 0 {
		return fmt.Errorf("invalid build_parser_version: must be non-negative")
	}
	if *s.BuildEpoch != s.ActiveEpoch+1 {
		return fmt.Errorf("invalid build_epoch: must equal active_epoch + 1")
	}
	return nil
}

func (s SourceUsageEpochState) WorkingEpoch() int64 {
	if s.BuildEpoch != nil {
		return *s.BuildEpoch
	}
	return s.ActiveEpoch
}

func (s SourceUsageEpochState) WorkingParserVersion() int64 {
	if s.BuildParserVersion != nil {
		return *s.BuildParserVersion
	}
	return s.ActiveParserVersion
}
