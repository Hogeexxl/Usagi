package codex

import (
	"bufio"
	"bytes"
	"encoding/json"
	"io"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"
	"unicode"
)

const (
	maxSessionIndexLineBytes = 1024 * 1024
	maxSessionTitleBytes     = 16 * 1024
)

type SessionSourceStatus uint8

const (
	SessionSourceUnavailable SessionSourceStatus = iota
	SessionSourceComplete
	SessionSourcePartial
)

type SessionIndexDiagnostic struct {
	Code       string
	LineNumber int64
	ThreadID   string
	Field      string
}

type SessionNameFact struct {
	ThreadID    string
	ThreadName  string
	UpdatedAtMS *int64
}

type SessionNameSnapshot struct {
	Names       map[string]SessionNameFact
	Facts       []SessionNameFact
	Diagnostics []SessionIndexDiagnostic
	Status      SessionSourceStatus
}

func (s SessionNameSnapshot) Get(threadID string) (SessionNameFact, bool) {
	fact, ok := s.Names[threadID]
	return fact, ok
}

func ReadSessionIndex(path string) SessionNameSnapshot {
	file, err := os.Open(path)
	if err != nil {
		return unavailableSessionSnapshot("file_unavailable")
	}
	defer file.Close()
	return readSessionSnapshot(file)
}

func readSessionSnapshot(reader io.Reader) SessionNameSnapshot {
	snapshot := SessionNameSnapshot{Names: make(map[string]SessionNameFact), Status: SessionSourceComplete}
	buffered := bufio.NewReader(reader)
	lineNumber := int64(0)
	for {
		line, consumed, terminated, oversized, err := readSessionLine(buffered, maxSessionIndexLineBytes)
		if err != nil {
			return unavailableSessionSnapshot("file_unreadable")
		}
		if consumed == 0 && len(line) == 0 && !terminated {
			break
		}
		lineNumber++
		if !terminated {
			snapshot.Status = SessionSourcePartial
			snapshot.Diagnostics = append(snapshot.Diagnostics, SessionIndexDiagnostic{Code: "half_line", LineNumber: lineNumber})
			break
		}
		if oversized {
			snapshot.Diagnostics = append(snapshot.Diagnostics, SessionIndexDiagnostic{Code: "line_too_large", LineNumber: lineNumber})
			continue
		}
		line = bytes.TrimSuffix(line, []byte{'\n'})
		line = bytes.TrimSuffix(line, []byte{'\r'})
		if len(bytes.TrimSpace(line)) == 0 {
			continue
		}
		var row struct {
			ID         string          `json:"id"`
			ThreadID   string          `json:"thread_id"`
			ThreadName string          `json:"thread_name"`
			UpdatedAt  json.RawMessage `json:"updated_at"`
		}
		if err := json.Unmarshal(line, &row); err != nil {
			snapshot.Diagnostics = append(snapshot.Diagnostics, SessionIndexDiagnostic{Code: "invalid_json", LineNumber: lineNumber})
			continue
		}
		id := strings.TrimSpace(row.ID)
		if id == "" {
			id = strings.TrimSpace(row.ThreadID)
		}
		if !validMetadataID(id) {
			snapshot.Diagnostics = append(snapshot.Diagnostics, SessionIndexDiagnostic{Code: "invalid_id", LineNumber: lineNumber, Field: "id/thread_id"})
			continue
		}
		name := strings.TrimSpace(row.ThreadName)
		if name == "" {
			snapshot.Diagnostics = append(snapshot.Diagnostics, SessionIndexDiagnostic{Code: "missing_title", LineNumber: lineNumber, ThreadID: id, Field: "thread_name"})
			continue
		}
		if len(name) > maxSessionTitleBytes || hasUnicodeControl(name) {
			code := "invalid_title"
			if len(name) > maxSessionTitleBytes {
				code = "title_too_large"
			}
			snapshot.Diagnostics = append(snapshot.Diagnostics, SessionIndexDiagnostic{Code: code, LineNumber: lineNumber, ThreadID: id, Field: "thread_name"})
			continue
		}
		updatedAt, validTime := parseSessionUpdatedAt(row.UpdatedAt)
		if len(row.UpdatedAt) != 0 && string(row.UpdatedAt) != "null" && !validTime {
			snapshot.Diagnostics = append(snapshot.Diagnostics, SessionIndexDiagnostic{Code: "invalid_time", LineNumber: lineNumber, ThreadID: id, Field: "updated_at"})
		}
		candidate := SessionNameFact{ThreadID: id, ThreadName: name, UpdatedAtMS: updatedAt}
		current, exists := snapshot.Names[id]
		if !exists || shouldReplaceSessionName(current, candidate, &snapshot.Diagnostics, lineNumber) {
			snapshot.Names[id] = candidate
		}
	}
	snapshot.Facts = make([]SessionNameFact, 0, len(snapshot.Names))
	for _, fact := range snapshot.Names {
		snapshot.Facts = append(snapshot.Facts, fact)
	}
	sort.Slice(snapshot.Facts, func(i, j int) bool { return snapshot.Facts[i].ThreadID < snapshot.Facts[j].ThreadID })
	return snapshot
}

func unavailableSessionSnapshot(code string) SessionNameSnapshot {
	return SessionNameSnapshot{Names: map[string]SessionNameFact{}, Status: SessionSourceUnavailable,
		Diagnostics: []SessionIndexDiagnostic{{Code: code}}}
}

func readSessionLine(reader *bufio.Reader, maxBytes int) ([]byte, int, bool, bool, error) {
	var line []byte
	consumed := 0
	oversized := false
	for {
		part, err := reader.ReadSlice('\n')
		consumed += len(part)
		if !oversized {
			if len(line)+len(part) > maxBytes {
				line = nil
				oversized = true
			} else {
				line = append(line, part...)
			}
		}
		if err == nil {
			return line, consumed, true, oversized, nil
		}
		if err == bufio.ErrBufferFull {
			continue
		}
		if err == io.EOF {
			return line, consumed, false, oversized, nil
		}
		return nil, consumed, false, oversized, err
	}
}

func parseSessionUpdatedAt(raw json.RawMessage) (*int64, bool) {
	if len(raw) == 0 || string(raw) == "null" {
		return nil, true
	}
	var text string
	if json.Unmarshal(raw, &text) == nil {
		parsed, err := time.Parse(time.RFC3339Nano, strings.TrimSpace(text))
		if err != nil {
			return nil, false
		}
		millis := parsed.UnixMilli()
		if millis < 0 || millis > maxReasonableEpochMS {
			return nil, false
		}
		return &millis, true
	}
	var number json.Number
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	if decoder.Decode(&number) != nil {
		return nil, false
	}
	value, err := strconv.ParseInt(number.String(), 10, 64)
	if err != nil || value < 0 {
		return nil, false
	}
	if value < 100_000_000_000 {
		if value > maxReasonableEpochMS/1000 {
			return nil, false
		}
		value *= 1000
	}
	if value > maxReasonableEpochMS {
		return nil, false
	}
	return &value, true
}

func shouldReplaceSessionName(current, candidate SessionNameFact, diagnostics *[]SessionIndexDiagnostic, lineNumber int64) bool {
	switch {
	case current.UpdatedAtMS != nil && candidate.UpdatedAtMS != nil:
		if *candidate.UpdatedAtMS > *current.UpdatedAtMS {
			return true
		}
		if *candidate.UpdatedAtMS < *current.UpdatedAtMS {
			return false
		}
		if current.ThreadName == candidate.ThreadName {
			return false
		}
		*diagnostics = append(*diagnostics, SessionIndexDiagnostic{Code: "same_timestamp_conflict", LineNumber: lineNumber, ThreadID: candidate.ThreadID, Field: "thread_name"})
		return true
	case current.UpdatedAtMS != nil:
		return false
	case candidate.UpdatedAtMS != nil:
		return true
	default:
		return true
	}
}

func hasUnicodeControl(value string) bool {
	for _, r := range value {
		if unicode.IsControl(r) {
			return true
		}
	}
	return false
}
