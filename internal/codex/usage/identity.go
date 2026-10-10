package usage

import (
	"encoding/binary"
	"encoding/hex"

	"github.com/Hogeexxl/Usagi/internal/usage"
	"github.com/zeebo/blake3"
)

func UsageFingerprint(value usage.NormalizedTokenUsage) [32]byte {
	var input [65]byte
	offset := 0
	writeInt64 := func(value int64) {
		binary.BigEndian.PutUint64(input[offset:offset+8], uint64(value))
		offset += 8
	}

	writeInt64(UsageCanonicalAlgorithmVersion)
	writeInt64(value.InputTokens)
	writeInt64(value.CachedTokens)
	if value.CacheWriteTokens == nil {
		input[offset] = 0
		offset++
	} else {
		input[offset] = 1
		offset++
		writeInt64(*value.CacheWriteTokens)
	}
	writeInt64(value.OutputTokens)
	writeInt64(value.ReasoningTokens)
	writeInt64(value.TotalTokens)
	return blake3.Sum256(input[:offset])
}

func ResponseEventID(threadID, responseID string) string {
	encoder := newEncoder("codex-response-v6")
	encoder.text(threadID)
	encoder.text(responseID)
	return encoder.finish()
}

func LegacyEventID(
	threadID string,
	turnKey *string,
	eventKind byte,
	occurredAtMS int64,
	previousTotal *usage.NormalizedTokenUsage,
	currentTotal usage.NormalizedTokenUsage,
	vector usage.NormalizedTokenUsage,
	model string,
	reasoningEffort *string,
) string {
	encoder := newEncoder("usage-event-v2")
	encoder.text(threadID)
	encoder.optionalText(turnKey)
	encoder.byte(eventKind)
	encoder.i64(occurredAtMS)
	encoder.optionalFingerprint(previousTotal)
	encoder.fingerprint(currentTotal)
	encoder.vector(vector)
	encoder.text(model)
	encoder.optionalText(reasoningEffort)
	return encoder.finish()
}

func TurnKeyFor(threadID string, turnID *string, startOffset uint64, timestampMS *int64) string {
	if turnID != nil {
		return *turnID
	}
	encoder := newEncoder("synthetic-turn-v1")
	encoder.text(threadID)
	encoder.u64(startOffset)
	if timestampMS == nil {
		encoder.byte(0)
	} else {
		encoder.byte(1)
		encoder.i64(*timestampMS)
	}
	return encoder.finish()
}

type encoder struct {
	hasher *blake3.Hasher
}

func newEncoder(tag string) *encoder {
	encoder := &encoder{hasher: blake3.New()}
	encoder.u64(uint64(len(tag)))
	encoder.hasher.Write([]byte(tag))
	return encoder
}

func (encoder *encoder) byte(value byte) {
	encoder.hasher.Write([]byte{value})
}

func (encoder *encoder) u64(value uint64) {
	var bytes [8]byte
	binary.BigEndian.PutUint64(bytes[:], value)
	encoder.hasher.Write(bytes[:])
}

func (encoder *encoder) i64(value int64) {
	encoder.u64(uint64(value))
}

func (encoder *encoder) text(value string) {
	encoder.u64(uint64(len(value)))
	encoder.hasher.Write([]byte(value))
}

func (encoder *encoder) optionalText(value *string) {
	if value == nil {
		encoder.byte(0)
		return
	}
	encoder.byte(1)
	encoder.text(*value)
}

func (encoder *encoder) optionalFingerprint(value *usage.NormalizedTokenUsage) {
	if value == nil {
		encoder.byte(0)
		return
	}
	encoder.byte(1)
	encoder.fingerprint(*value)
}

func (encoder *encoder) fingerprint(value usage.NormalizedTokenUsage) {
	fingerprint := UsageFingerprint(value)
	encoder.hasher.Write(fingerprint[:])
}

func (encoder *encoder) vector(value usage.NormalizedTokenUsage) {
	encoder.i64(value.InputTokens)
	encoder.i64(value.CachedTokens)
	if value.CacheWriteTokens == nil {
		encoder.byte(0)
	} else {
		encoder.byte(1)
		encoder.i64(*value.CacheWriteTokens)
	}
	encoder.i64(value.OutputTokens)
	encoder.i64(value.ReasoningTokens)
	encoder.i64(value.TotalTokens)
}

func (encoder *encoder) finish() string {
	return hex.EncodeToString(encoder.hasher.Sum(nil))
}
