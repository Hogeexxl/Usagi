package codex

import (
	"bytes"
	"encoding/json"
	"fmt"
	"sort"
	"strings"
	"time"
	"unicode"
	"unicode/utf8"

	"github.com/Hogeexxl/Usagi/internal/codex/rollout"
	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

type SkillEvent struct {
	SourceFileID  int64
	Generation    int64
	StartOffset   int64
	EndOffset     int64
	OccurredAtMS  int64
	ThreadID      string
	RootSessionID string
	Model         *string
	SkillName     string
}

func ExtractSkillEvents(record rollout.Record, ownership rollout.Ownership, rootSessionID string, model *string) []SkillEvent {
	if !bytes.Contains(record.JSON, []byte("SKILL.md")) {
		return nil
	}
	if ownership.Kind != rollout.OwnershipOwning || ownership.ThreadID == "" || rootSessionID == "" {
		return nil
	}

	var envelope struct {
		Type      string          `json:"type"`
		Timestamp json.RawMessage `json:"timestamp"`
		Payload   struct {
			Type      string          `json:"type"`
			Name      string          `json:"name"`
			Timestamp json.RawMessage `json:"timestamp"`
			Arguments json.RawMessage `json:"arguments"`
			Input     json.RawMessage `json:"input"`
			Action    json.RawMessage `json:"action"`
		} `json:"payload"`
	}
	if json.Unmarshal(record.JSON, &envelope) != nil || envelope.Type != "response_item" {
		return nil
	}
	occurredAtMS, ok := parseSkillTimestamp(envelope.Payload.Timestamp)
	if !ok {
		occurredAtMS, ok = parseSkillTimestamp(envelope.Timestamp)
	}
	if !ok || !strings.HasSuffix(envelope.Payload.Type, "_call") {
		return nil
	}

	names := make(map[string]struct{})
	switch envelope.Payload.Type {
	case "function_call":
		if envelope.Payload.Name == "exec_command" {
			collectFunctionSkillCommands(envelope.Payload.Arguments, names)
		}
	case "custom_tool_call":
		if envelope.Payload.Name == "exec" {
			var input string
			if json.Unmarshal(envelope.Payload.Input, &input) == nil {
				collectJavaScriptSkillCommands(input, names)
			}
		}
	case "local_shell_call":
		var action map[string]json.RawMessage
		if json.Unmarshal(envelope.Payload.Action, &action) == nil {
			collectCommandFields(action, names)
		}
	}
	if len(names) == 0 {
		return nil
	}

	sortedNames := make([]string, 0, len(names))
	for name := range names {
		sortedNames = append(sortedNames, name)
	}
	sort.Strings(sortedNames)
	events := make([]SkillEvent, 0, len(sortedNames))
	for _, name := range sortedNames {
		event := SkillEvent{
			SourceFileID:  record.SourceFileID,
			Generation:    record.Generation,
			StartOffset:   record.LogicalStartOffset,
			EndOffset:     record.LogicalEndOffset,
			OccurredAtMS:  occurredAtMS,
			ThreadID:      ownership.ThreadID,
			RootSessionID: rootSessionID,
			SkillName:     name,
		}
		if model != nil {
			modelCopy := *model
			event.Model = &modelCopy
		}
		events = append(events, event)
	}
	return events
}

func WriteSkillEvents(tx *source.WriteTx, target source.UsageWriteTarget, events []SkillEvent, committedAtMS int64) (bool, error) {
	if len(events) == 0 {
		return false, nil
	}
	if committedAtMS < 0 {
		return false, fmt.Errorf("invalid skill commit timestamp")
	}
	epoch, err := tx.ResolveUsageWriteEpoch(target)
	if err != nil {
		return false, err
	}
	ordered := append([]SkillEvent(nil), events...)
	sort.Slice(ordered, func(i, j int) bool {
		a, b := ordered[i], ordered[j]
		if a.SourceFileID != b.SourceFileID {
			return a.SourceFileID < b.SourceFileID
		}
		if a.Generation != b.Generation {
			return a.Generation < b.Generation
		}
		if a.StartOffset != b.StartOffset {
			return a.StartOffset < b.StartOffset
		}
		return a.SkillName < b.SkillName
	})
	visibleChanged := false
	err = tx.Private(func(private storage.PrivateTx) error {
		for _, event := range ordered {
			if event.SourceFileID <= 0 || event.Generation <= 0 || event.StartOffset < 0 || event.EndOffset <= event.StartOffset || event.OccurredAtMS < 0 ||
				event.ThreadID == "" || event.RootSessionID == "" || !validSkillName(event.SkillName) {
				return fmt.Errorf("invalid skill event")
			}
			result, err := private.Exec(`INSERT OR IGNORE INTO codex_skill_usage_events(
				ledger_epoch,source_file_id,file_generation,source_start_offset,source_end_offset,
				occurred_at_ms,thread_id,root_session_id,model,skill_name,created_at_ms
			) VALUES(?,?,?,?,?,?,?,?,?,?,?)`, epoch, event.SourceFileID, event.Generation, event.StartOffset,
				event.EndOffset, event.OccurredAtMS, event.ThreadID, event.RootSessionID, event.Model,
				event.SkillName, committedAtMS)
			if err != nil {
				return err
			}
			rows, err := result.RowsAffected()
			if err != nil {
				return err
			}
			visibleChanged = visibleChanged || rows != 0
		}
		return nil
	})
	return visibleChanged, err
}

func collectFunctionSkillCommands(arguments json.RawMessage, output map[string]struct{}) {
	var text string
	if len(arguments) != 0 && arguments[0] == '"' {
		if json.Unmarshal(arguments, &text) != nil {
			return
		}
		arguments = []byte(text)
	}
	var object map[string]json.RawMessage
	if json.Unmarshal(arguments, &object) == nil {
		collectCommandFields(object, output)
	}
}

func collectCommandFields(object map[string]json.RawMessage, output map[string]struct{}) {
	for _, key := range []string{"cmd", "command"} {
		var text string
		if json.Unmarshal(object[key], &text) == nil {
			extractSkillLocators(text, output)
			continue
		}
		var values []json.RawMessage
		if json.Unmarshal(object[key], &values) == nil {
			for _, raw := range values {
				if json.Unmarshal(raw, &text) == nil {
					extractSkillLocators(text, output)
				}
			}
		}
	}
}

func collectJavaScriptSkillCommands(input string, output map[string]struct{}) {
	for cursor := 0; cursor < len(input); {
		start := findJavaScriptCodeMarker(input, "tools.exec_command", cursor)
		if start < 0 {
			return
		}
		end := start + len("tools.exec_command")
		if start > 0 && isJavaScriptIdentifierByte(input[start-1]) || end < len(input) && isJavaScriptIdentifierByte(input[end]) {
			cursor = end
			continue
		}
		open := skipJavaScriptSpaceAndComments(input, end)
		if open >= len(input) || input[open] != '(' {
			cursor = end
			continue
		}
		close := findJavaScriptCallEnd(input, open)
		if close < 0 {
			return
		}
		if command, ok := extractJavaScriptCallCommand(input[open+1 : close]); ok {
			extractSkillLocators(command, output)
		}
		cursor = close + 1
	}
}

func findJavaScriptCodeMarker(input, marker string, from int) int {
	for index := from; index < len(input); {
		next, state := skipJavaScriptNonCode(input, index)
		if state {
			index = next
			continue
		}
		if strings.HasPrefix(input[index:], marker) {
			return index
		}
		index++
	}
	return -1
}

func skipJavaScriptNonCode(input string, index int) (int, bool) {
	if index >= len(input) {
		return index, false
	}
	bytes := []byte(input)
	switch bytes[index] {
	case '\'', '"', '`':
		return skipJavaScriptString(input, index), true
	case '/':
		if index+1 < len(bytes) && bytes[index+1] == '/' {
			end := strings.IndexByte(input[index+2:], '\n')
			if end < 0 {
				return len(input), true
			}
			return index + 2 + end + 1, true
		}
		if index+1 < len(bytes) && bytes[index+1] == '*' {
			end := strings.Index(input[index+2:], "*/")
			if end < 0 {
				return len(input), true
			}
			return index + 2 + end + 2, true
		}
	}
	return index, false
}

func skipJavaScriptString(input string, start int) int {
	delimiter := input[start]
	escaped := false
	for index := start + 1; index < len(input); index++ {
		if escaped {
			escaped = false
			continue
		}
		if input[index] == '\\' {
			escaped = true
			continue
		}
		if input[index] == delimiter {
			return index + 1
		}
	}
	return len(input)
}

func skipJavaScriptSpaceAndComments(input string, index int) int {
	for index < len(input) {
		if input[index] == ' ' || input[index] == '\t' || input[index] == '\r' || input[index] == '\n' {
			index++
			continue
		}
		next, skipped := skipJavaScriptNonCode(input, index)
		if skipped && input[index] == '/' {
			index = next
			continue
		}
		return index
	}
	return index
}

func findJavaScriptCallEnd(input string, open int) int {
	depth := 0
	for index := open; index < len(input); {
		next, skipped := skipJavaScriptNonCode(input, index)
		if skipped {
			index = next
			continue
		}
		switch input[index] {
		case '(':
			depth++
		case ')':
			depth--
			if depth == 0 {
				return index
			}
		}
		index++
	}
	return -1
}

func extractJavaScriptCallCommand(arguments string) (string, bool) {
	index := skipJavaScriptSpaceAndComments(arguments, 0)
	if index >= len(arguments) || arguments[index] != '{' {
		return "", false
	}
	index++
	for index < len(arguments) {
		index = skipJavaScriptSpaceAndComments(arguments, index)
		for index < len(arguments) && arguments[index] == ',' {
			index++
			index = skipJavaScriptSpaceAndComments(arguments, index)
		}
		if index >= len(arguments) || arguments[index] == '}' {
			return "", false
		}
		key, afterKey, ok := readJavaScriptPropertyKey(arguments, index)
		if !ok {
			index = skipJavaScriptObjectValue(arguments, index)
			continue
		}
		colon := skipJavaScriptSpaceAndComments(arguments, afterKey)
		if colon >= len(arguments) || arguments[colon] != ':' {
			index = skipJavaScriptObjectValue(arguments, index)
			continue
		}
		value := skipJavaScriptSpaceAndComments(arguments, colon+1)
		if value < len(arguments) && (arguments[value] == '\'' || arguments[value] == '"' || arguments[value] == '`') {
			text, end, valid := readJavaScriptString(arguments, value)
			if valid && key == "cmd" {
				return text, true
			}
			index = end
			continue
		}
		index = skipJavaScriptObjectValue(arguments, value)
	}
	return "", false
}

func readJavaScriptPropertyKey(input string, start int) (string, int, bool) {
	if start >= len(input) {
		return "", start, false
	}
	if input[start] == '\'' || input[start] == '"' {
		return readJavaScriptString(input, start)
	}
	end := start
	for end < len(input) && isJavaScriptIdentifierByte(input[end]) {
		end++
	}
	if end == start {
		return "", start, false
	}
	return input[start:end], end, true
}

func readJavaScriptString(input string, start int) (string, int, bool) {
	if start >= len(input) || !strings.ContainsRune("'\"`", rune(input[start])) {
		return "", start, false
	}
	delimiter := input[start]
	var value strings.Builder
	for index := start + 1; index < len(input); index++ {
		if input[index] == delimiter {
			return value.String(), index + 1, true
		}
		if input[index] == '\\' {
			index++
			if index >= len(input) {
				return "", len(input), false
			}
			switch input[index] {
			case 'n':
				value.WriteByte('\n')
			case 'r':
				value.WriteByte('\r')
			case 't':
				value.WriteByte('\t')
			default:
				value.WriteByte(input[index])
			}
			continue
		}
		value.WriteByte(input[index])
	}
	return "", len(input), false
}

func skipJavaScriptObjectValue(input string, start int) int {
	depthObject, depthArray, depthParen := 0, 0, 0
	for index := start; index < len(input); {
		next, skipped := skipJavaScriptNonCode(input, index)
		if skipped {
			index = next
			continue
		}
		switch input[index] {
		case '{':
			depthObject++
		case '}':
			if depthObject == 0 && depthArray == 0 && depthParen == 0 {
				return index
			}
			depthObject--
		case '[':
			depthArray++
		case ']':
			depthArray--
		case '(':
			depthParen++
		case ')':
			depthParen--
		case ',':
			if depthObject == 0 && depthArray == 0 && depthParen == 0 {
				return index + 1
			}
		}
		index++
	}
	return len(input)
}

func isJavaScriptIdentifierByte(value byte) bool {
	return value == '_' || value == '$' || value >= 'a' && value <= 'z' || value >= 'A' && value <= 'Z' || value >= '0' && value <= '9'
}

func extractSkillLocators(text string, output map[string]struct{}) {
	normalized := strings.ReplaceAll(text, `\`, "/")
	for cursor := 0; cursor < len(normalized); {
		relative := strings.Index(normalized[cursor:], "SKILL.md")
		if relative < 0 {
			return
		}
		index := cursor + relative
		after := index + len("SKILL.md")
		beforeBoundary := index > 0 && normalized[index-1] == '/'
		afterBoundary := after == len(normalized) || normalized[after] != '/' && !isSkillPathComponentByte(normalized[after])
		if beforeBoundary && afterBoundary {
			components := strings.Split(strings.Trim(normalized[:index], "/"), "/")
			if len(components) >= 2 {
				skill := components[len(components)-1]
				skillsIndex := len(components) - 2
				if validSkillName(skill) && components[skillsIndex] == "skills" {
					name := skill
					prefix := components[:skillsIndex]
					if len(prefix) >= 4 && prefix[len(prefix)-4] == "plugins" && prefix[len(prefix)-3] == "cache" && validNamespaceComponent(prefix[len(prefix)-2]) && validNamespaceComponent(prefix[len(prefix)-1]) {
						name = prefix[len(prefix)-2] + ":" + skill
					}
					if validSkillName(name) {
						output[name] = struct{}{}
					}
				}
			}
		}
		cursor = after
	}
}

func isSkillPathComponentByte(value byte) bool {
	return value >= 'a' && value <= 'z' || value >= 'A' && value <= 'Z' || value >= '0' && value <= '9' || value == '_' || value == '-' || value == '.'
}

func validSkillName(value string) bool {
	if value == "" || value == "." || value == ".." || len(value) > 128 || !utf8.ValidString(value) {
		return false
	}
	for _, r := range value {
		if unicode.IsControl(r) || r == '/' || r == '\\' {
			return false
		}
	}
	return true
}

func validNamespaceComponent(value string) bool {
	if value == "" || !utf8.ValidString(value) {
		return false
	}
	for _, r := range value {
		if unicode.IsControl(r) {
			return false
		}
	}
	return true
}

func parseSkillTimestamp(raw json.RawMessage) (int64, bool) {
	if len(raw) == 0 {
		return 0, false
	}
	var numeric int64
	if json.Unmarshal(raw, &numeric) == nil {
		return numeric, numeric >= 0
	}
	var text string
	if json.Unmarshal(raw, &text) != nil {
		return 0, false
	}
	parsed, err := time.Parse(time.RFC3339Nano, text)
	if err != nil {
		return 0, false
	}
	value := parsed.UnixMilli()
	return value, value >= 0
}
