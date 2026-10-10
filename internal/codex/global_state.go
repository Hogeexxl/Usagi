package codex

import (
	"encoding/json"
	"errors"
	"io"
	"os"
	"strings"
)

type GlobalStateStatus uint8

const (
	GlobalStateComplete GlobalStateStatus = iota + 1
	GlobalStateNotPresent
	GlobalStateMalformed
	GlobalStateUnreadable
)

type GlobalStateDiagnostic struct {
	Code  string
	Field string
}

type GlobalStateSnapshot struct {
	Status                   GlobalStateStatus
	ProjectlessThreadIDs     []string
	ThreadProjectAssignments map[string]struct{}
	Diagnostics              []GlobalStateDiagnostic
}

func (s GlobalStateSnapshot) IsProjectless(threadID string) bool {
	for _, id := range s.ProjectlessThreadIDs {
		if id == threadID {
			return true
		}
	}
	return false
}

func (s GlobalStateSnapshot) HasAssignment(threadID string) bool {
	_, ok := s.ThreadProjectAssignments[threadID]
	return ok
}

func ReadGlobalState(path string) GlobalStateSnapshot {
	file, err := os.Open(path)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return malformedGlobalState(GlobalStateNotPresent, "file_not_present", "")
		}
		return malformedGlobalState(GlobalStateUnreadable, "file_unreadable", "")
	}
	defer file.Close()
	return readGlobalState(file)
}

func readGlobalState(reader io.Reader) GlobalStateSnapshot {
	decoder := json.NewDecoder(reader)
	root, err := decoder.Token()
	if err != nil {
		return globalDecodeFailure(err, "invalid_json", "")
	}
	if root != json.Delim('{') {
		return malformedGlobalState(GlobalStateMalformed, "invalid_json", "")
	}
	var projectless []string
	var assignments map[string]struct{}
	projectlessFound, assignmentsFound := false, false
	diagnostics := make([]GlobalStateDiagnostic, 0)
	for decoder.More() {
		keyToken, err := decoder.Token()
		if err != nil {
			return globalDecodeFailure(err, "invalid_json", "")
		}
		key, ok := keyToken.(string)
		if !ok {
			return malformedGlobalState(GlobalStateMalformed, "invalid_json", "")
		}
		switch key {
		case "projectless-thread-ids":
			if projectlessFound {
				return malformedGlobalState(GlobalStateMalformed, "duplicate_field", key)
			}
			projectlessFound = true
			projectless, diagnostics, err = readProjectlessIDs(decoder, diagnostics)
			if err != nil {
				return globalDecodeFailure(err, "invalid_projectless_ids", key)
			}
		case "thread-project-assignments":
			if assignmentsFound {
				return malformedGlobalState(GlobalStateMalformed, "duplicate_field", key)
			}
			assignmentsFound = true
			var duplicateDiagnostics []GlobalStateDiagnostic
			assignments, duplicateDiagnostics, err = readAssignmentIDs(decoder)
			if err != nil {
				return globalDecodeFailure(err, "invalid_assignments", key)
			}
			diagnostics = append(diagnostics, duplicateDiagnostics...)
		default:
			if err := discardJSONValue(decoder); err != nil {
				return globalDecodeFailure(err, "invalid_json", "")
			}
		}
	}
	if _, err := decoder.Token(); err != nil {
		return globalDecodeFailure(err, "invalid_json", "")
	}
	if !projectlessFound {
		return malformedGlobalState(GlobalStateMalformed, "missing_field", "projectless-thread-ids")
	}
	if !assignmentsFound {
		return malformedGlobalState(GlobalStateMalformed, "missing_field", "thread-project-assignments")
	}
	if _, err := decoder.Token(); err != io.EOF {
		if err != nil {
			return globalDecodeFailure(err, "trailing_data", "")
		}
		return malformedGlobalState(GlobalStateMalformed, "trailing_data", "")
	}
	return GlobalStateSnapshot{Status: GlobalStateComplete, ProjectlessThreadIDs: projectless,
		ThreadProjectAssignments: assignments, Diagnostics: diagnostics}
}

var errInvalidGlobalShape = errors.New("invalid global state shape")

func readProjectlessIDs(decoder *json.Decoder, diagnostics []GlobalStateDiagnostic) ([]string, []GlobalStateDiagnostic, error) {
	token, err := decoder.Token()
	if err != nil {
		return nil, diagnostics, err
	}
	if token != json.Delim('[') {
		return nil, diagnostics, errInvalidGlobalShape
	}
	seen := make(map[string]struct{})
	ids := make([]string, 0)
	for decoder.More() {
		value, err := decoder.Token()
		if err != nil {
			return nil, diagnostics, err
		}
		id, ok := value.(string)
		id = strings.TrimSpace(id)
		if !ok || !validMetadataID(id) {
			return nil, diagnostics, errInvalidGlobalShape
		}
		if _, duplicate := seen[id]; duplicate {
			diagnostics = append(diagnostics, GlobalStateDiagnostic{Code: "duplicate_thread_id", Field: "projectless-thread-ids"})
			continue
		}
		seen[id] = struct{}{}
		ids = append(ids, id)
	}
	if _, err := decoder.Token(); err != nil {
		return nil, diagnostics, err
	}
	return ids, diagnostics, nil
}

func readAssignmentIDs(decoder *json.Decoder) (map[string]struct{}, []GlobalStateDiagnostic, error) {
	token, err := decoder.Token()
	if err != nil {
		return nil, nil, err
	}
	if token != json.Delim('{') {
		return nil, nil, errInvalidGlobalShape
	}
	ids := make(map[string]struct{})
	var diagnostics []GlobalStateDiagnostic
	for decoder.More() {
		keyToken, err := decoder.Token()
		if err != nil {
			return nil, nil, err
		}
		id, ok := keyToken.(string)
		id = strings.TrimSpace(id)
		if !ok || !validMetadataID(id) {
			return nil, nil, errInvalidGlobalShape
		}
		if _, duplicate := ids[id]; duplicate {
			diagnostics = append(diagnostics, GlobalStateDiagnostic{Code: "duplicate_thread_id", Field: "thread-project-assignments"})
			if err := discardJSONValue(decoder); err != nil {
				return nil, nil, err
			}
			continue
		}
		if err := discardJSONValue(decoder); err != nil {
			return nil, nil, err
		}
		ids[id] = struct{}{}
	}
	if _, err := decoder.Token(); err != nil {
		return nil, nil, err
	}
	return ids, diagnostics, nil
}

func discardJSONValue(decoder *json.Decoder) error {
	token, err := decoder.Token()
	if err != nil {
		return err
	}
	delim, ok := token.(json.Delim)
	if !ok || (delim != '{' && delim != '[') {
		return nil
	}
	end := json.Delim(']')
	if delim == '{' {
		end = '}'
		for decoder.More() {
			if _, err := decoder.Token(); err != nil {
				return err
			}
			if err := discardJSONValue(decoder); err != nil {
				return err
			}
		}
	} else {
		for decoder.More() {
			if err := discardJSONValue(decoder); err != nil {
				return err
			}
		}
	}
	closeToken, err := decoder.Token()
	if err != nil {
		return err
	}
	if closeToken != end {
		return errInvalidGlobalShape
	}
	return nil
}

func globalDecodeFailure(err error, code, field string) GlobalStateSnapshot {
	if errors.Is(err, errInvalidGlobalShape) || errors.Is(err, io.EOF) || errors.Is(err, io.ErrUnexpectedEOF) {
		return malformedGlobalState(GlobalStateMalformed, code, field)
	}
	var syntaxError *json.SyntaxError
	if errors.As(err, &syntaxError) {
		return malformedGlobalState(GlobalStateMalformed, code, field)
	}
	return malformedGlobalState(GlobalStateUnreadable, "file_unreadable", field)
}

func malformedGlobalState(status GlobalStateStatus, code, field string) GlobalStateSnapshot {
	return GlobalStateSnapshot{Status: status, ThreadProjectAssignments: map[string]struct{}{},
		Diagnostics: []GlobalStateDiagnostic{{Code: code, Field: field}}}
}
