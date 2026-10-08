package domain

import (
	"strings"
	"testing"
)

func TestSourceUsageEpochValidation(t *testing.T) {
	buildEpoch := int64(4)
	buildParser := int64(2)
	valid := []struct {
		name                string
		source              SourceID
		activeEpoch         int64
		buildEpoch          *int64
		activeParserVersion int64
		buildParserVersion  *int64
	}{
		{"initial", SourceCodex, 0, nil, 0, nil},
		{"active only", SourceAntigravity, 3, nil, 2, nil},
		{"active and adjacent build", SourceCodex, 3, &buildEpoch, 1, &buildParser},
	}
	for _, test := range valid {
		t.Run(test.name, func(t *testing.T) {
			state, err := NewSourceUsageEpochState(
				test.source,
				test.activeEpoch,
				test.buildEpoch,
				test.activeParserVersion,
				test.buildParserVersion,
			)
			if err != nil {
				t.Fatalf("NewSourceUsageEpochState: %v", err)
			}
			if err := state.Validate(); err != nil {
				t.Fatalf("Validate: %v", err)
			}
		})
	}

	invalid := []struct {
		name                string
		source              SourceID
		activeEpoch         int64
		buildEpoch          *int64
		activeParserVersion int64
		buildParserVersion  *int64
	}{
		{"invalid source", SourceID("Codex"), 0, nil, 0, nil},
		{"negative active epoch", SourceCodex, -1, nil, 0, nil},
		{"negative active parser", SourceCodex, 0, nil, -1, nil},
		{"build epoch without parser", SourceCodex, 3, &buildEpoch, 1, nil},
		{"build parser without epoch", SourceCodex, 3, nil, 1, &buildParser},
		{"zero build epoch", SourceCodex, 0, int64Ptr(0), 0, int64Ptr(0)},
		{"negative build parser", SourceCodex, 3, &buildEpoch, 1, int64Ptr(-1)},
		{"non-adjacent build epoch", SourceCodex, 3, int64Ptr(5), 1, &buildParser},
	}
	for _, test := range invalid {
		t.Run(test.name, func(t *testing.T) {
			if _, err := NewSourceUsageEpochState(
				test.source,
				test.activeEpoch,
				test.buildEpoch,
				test.activeParserVersion,
				test.buildParserVersion,
			); err == nil {
				t.Fatal("NewSourceUsageEpochState succeeded")
			}
		})
	}

	state, err := NewSourceUsageEpochState(SourceCodex, 0, nil, 0, nil)
	if err != nil {
		t.Fatal(err)
	}
	state.ActiveEpoch = -1
	if err := state.Validate(); err == nil {
		t.Fatal("Validate accepted a negative active epoch")
	}

	if _, err := NewSourceUsageEpochState(SourceID(strings.Repeat("a", 65)), 0, nil, 0, nil); err == nil {
		t.Fatal("NewSourceUsageEpochState accepted an invalid source ID")
	}
}

func TestSourceUsageEpochWorkingValues(t *testing.T) {
	withoutBuild, err := NewSourceUsageEpochState(SourceCodex, 3, nil, 2, nil)
	if err != nil {
		t.Fatal(err)
	}
	if withoutBuild.WorkingEpoch() != 3 || withoutBuild.WorkingParserVersion() != 2 {
		t.Fatalf("working values without build = (%d, %d)", withoutBuild.WorkingEpoch(), withoutBuild.WorkingParserVersion())
	}

	buildEpoch, buildParser := int64(4), int64(5)
	withBuild, err := NewSourceUsageEpochState(SourceCodex, 3, &buildEpoch, 2, &buildParser)
	if err != nil {
		t.Fatal(err)
	}
	if withBuild.WorkingEpoch() != buildEpoch || withBuild.WorkingParserVersion() != buildParser {
		t.Fatalf("working values with build = (%d, %d)", withBuild.WorkingEpoch(), withBuild.WorkingParserVersion())
	}
}

func int64Ptr(value int64) *int64 {
	return &value
}
