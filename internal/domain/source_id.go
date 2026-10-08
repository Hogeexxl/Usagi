package domain

import "fmt"

type SourceID string

const (
	SourceCodex       SourceID = "codex"
	SourceAntigravity SourceID = "antigravity"
)

func NewSourceID(value string) (SourceID, error) {
	id := SourceID(value)
	if err := id.Validate(); err != nil {
		return "", err
	}
	return id, nil
}

func (id SourceID) Validate() error {
	return validateSourceIDString(string(id))
}

func validateSourceIDString(value string) error {
	if len(value) == 0 {
		return fmt.Errorf("source id must not be empty")
	}
	if len(value) > 64 {
		return fmt.Errorf("source id must be at most 64 bytes")
	}
	if value[0] < 'a' || value[0] > 'z' {
		return fmt.Errorf("source id must start with an ASCII lowercase letter")
	}
	for _, value := range []byte(value[1:]) {
		if (value < 'a' || value > 'z') &&
			(value < '0' || value > '9') && value != '_' && value != '-' {
			return fmt.Errorf("source id must contain only lowercase ASCII letters, digits, '_' or '-'")
		}
	}
	return nil
}
