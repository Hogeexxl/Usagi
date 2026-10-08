package ingestion

import (
	"testing"
	"time"
)

func TestPolicyIntervalBounds(t *testing.T) {
	if got := DefaultConfig(); got.Interval != DefaultInterval || DefaultInterval != 300*time.Second {
		t.Fatalf("default config = %+v", got)
	}
	for _, test := range []struct {
		interval time.Duration
		valid    bool
	}{
		{59 * time.Second, false},
		{60 * time.Second, true},
		{3600 * time.Second, true},
		{3601 * time.Second, false},
	} {
		err := (Config{Interval: test.interval}).Validate()
		if (err == nil) != test.valid {
			t.Errorf("Validate(%s) error = %v, want valid=%t", test.interval, err, test.valid)
		}
	}
}

func TestRequestErrorContract(t *testing.T) {
	var _ error = RequestError{}
	for _, test := range []struct {
		kind       RequestErrorKind
		failure    CommitFailureKind
		wantCommit bool
	}{
		{RequestErrorRecovering, CommitFailureBusy, false},
		{RequestErrorShuttingDown, CommitFailureInternal, false},
		{RequestErrorStartCommitFailed, CommitFailureBusy, true},
		{RequestErrorStartCommitFailed, CommitFailureInternal, true},
		{RequestErrorEnqueueCommitFailed, CommitFailureBusy, true},
		{RequestErrorEnqueueCommitFailed, CommitFailureInternal, true},
	} {
		err := newRequestError(test.kind, test.failure)
		if err.Error() == "" || (err.CommitFailure != nil) != test.wantCommit {
			t.Errorf("RequestError(%d) = %+v", test.kind, err)
		}
		if err.CommitFailure != nil && *err.CommitFailure != CommitFailureBusy && *err.CommitFailure != CommitFailureInternal {
			t.Errorf("invalid CommitFailure: %+v", err)
		}
	}
	var err error = newRequestError(RequestErrorRecovering, 0)
	if _, ok := err.(RequestError); !ok {
		t.Fatalf("RequestError does not satisfy error: %T", err)
	}
}
