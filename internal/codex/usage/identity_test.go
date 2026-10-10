package usage

import (
	"encoding/hex"
	"testing"

	sharedusage "github.com/Hogeexxl/Usagi/internal/usage"
)

func TestIdentityRustOracleGoldens(t *testing.T) {
	current := identityUsage(t, 1_200, 300, identityInt64(100), 200, 50, 1_400)
	previous := identityUsage(t, 800, 200, identityInt64(50), 100, 20, 900)
	delta := identityUsage(t, 400, 100, identityInt64(50), 100, 30, 500)

	fingerprint := UsageFingerprint(current)
	if got := hex.EncodeToString(fingerprint[:]); got != "dd7c464d7fda75ca76b474c33b8097e4c1eb0c31cbd9f637eb0c35e96d9b3f7f" {
		t.Errorf("UsageFingerprint() = %s", got)
	}
	if got := ResponseEventID("thread-oracle", "response-oracle"); got != "bff09a0a5b68dec20c29ea9326ce4a8e67e263eaa00ee97ce806e7d0b06eb7d0" {
		t.Errorf("ResponseEventID() = %s", got)
	}
	turnKey := "turn-legacy"
	effort := "high"
	if got := LegacyEventID(
		"thread-legacy",
		&turnKey,
		1,
		1_700_000_000_123,
		&previous,
		current,
		delta,
		"gpt-5-codex",
		&effort,
	); got != "362f4996248881e86d35500c30d1b790e0fb107ae746288d7a1709fa1c91512c" {
		t.Errorf("LegacyEventID() = %s", got)
	}
	timestamp := int64(1_700_000_000_123)
	if got := TurnKeyFor("thread-turn", nil, 123_456, &timestamp); got != "359885e47d0fa8e3a2e4fa3101b7d069b8b17980cc5aae03ab5321b748f03f73" {
		t.Errorf("TurnKeyFor() = %s", got)
	}
}

func TestTurnKeyForUsesRawTurnID(t *testing.T) {
	turnID := "raw-turn-id"
	if got := TurnKeyFor("thread", &turnID, 99, nil); got != turnID {
		t.Fatalf("TurnKeyFor() = %q, want raw ID %q", got, turnID)
	}
}

func identityUsage(
	t *testing.T,
	input, cached int64,
	cacheWrite *int64,
	output, reasoning, total int64,
) sharedusage.NormalizedTokenUsage {
	t.Helper()
	value, err := sharedusage.NewNormalizedTokenUsage(input, cached, cacheWrite, output, reasoning, total)
	if err != nil {
		t.Fatal(err)
	}
	return value
}

func identityInt64(value int64) *int64 {
	return &value
}
