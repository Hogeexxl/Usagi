package source

import (
	"context"
	"errors"

	"github.com/Hogeexxl/Usagi/internal/domain"
)

type AvailabilityKind uint8

const (
	AvailabilityInvalid AvailabilityKind = iota
	AvailabilityAvailable
	AvailabilityUnavailable
	AvailabilityNotInstalled
)

type Availability struct {
	kind   AvailabilityKind
	reason string
}

func Available() Availability {
	return Availability{kind: AvailabilityAvailable}
}

func Unavailable(reason string) Availability {
	return Availability{kind: AvailabilityUnavailable, reason: reason}
}

func NotInstalled() Availability {
	return Availability{kind: AvailabilityNotInstalled}
}

func (a Availability) Kind() AvailabilityKind {
	return a.kind
}

func (a Availability) Reason() string {
	if a.kind != AvailabilityUnavailable {
		return ""
	}
	return a.reason
}

type Adapter interface {
	Descriptor() Descriptor
	Availability(ctx context.Context) (Availability, error)
	RunScan(ctx context.Context, run RunContext) error
}

type AdapterError struct {
	code    string
	message string
	err     error
}

func NewAdapterError(message string, err error) *AdapterError {
	return NewAdapterErrorWithCode("SOURCE_RUN_FAILED", message, err)
}

func NewAdapterErrorWithCode(code string, message string, err error) *AdapterError {
	return &AdapterError{code: code, message: message, err: err}
}

func (e *AdapterError) Code() string {
	if e == nil {
		return ""
	}
	return e.code
}

func (e *AdapterError) Error() string {
	if e == nil {
		return ""
	}
	if e.message != "" {
		return e.message
	}
	if e.err != nil {
		return e.err.Error()
	}
	return ""
}

func (e *AdapterError) Unwrap() error {
	if e == nil {
		return nil
	}
	return e.err
}

func ErrorCode(err error) string {
	var adapterError *AdapterError
	if errors.As(err, &adapterError) && adapterError != nil {
		return adapterError.Code()
	}
	return "SOURCE_RUN_FAILED"
}

type RunState uint8

const (
	RunCompleted RunState = iota
	RunSkipped
	RunFailed
)

type RunReport struct {
	ScanID    string
	Source    domain.SourceID
	State     RunState
	ErrorCode *string
	Detail    string
}
