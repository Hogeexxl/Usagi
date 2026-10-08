package source

import (
	"fmt"
	"strings"
	"unicode"

	"github.com/Hogeexxl/Usagi/internal/domain"
)

type Descriptor struct {
	ID          domain.SourceID
	DisplayName string
}

func NewDescriptor(id domain.SourceID, displayName string) (Descriptor, error) {
	descriptor := Descriptor{ID: id, DisplayName: displayName}
	if err := descriptor.Validate(); err != nil {
		return Descriptor{}, err
	}
	return descriptor, nil
}

func (d Descriptor) Validate() error {
	if err := d.ID.Validate(); err != nil {
		return fmt.Errorf("invalid source descriptor id: %w", err)
	}
	if strings.TrimSpace(d.DisplayName) == "" {
		return fmt.Errorf("source display name must not be empty")
	}
	for _, r := range d.DisplayName {
		if unicode.IsControl(r) {
			return fmt.Errorf("source display name must not contain control characters")
		}
	}
	return nil
}

func (d Descriptor) DisplayOrder() uint16 {
	switch d.ID {
	case domain.SourceCodex:
		return 10
	case domain.SourceAntigravity:
		return 20
	default:
		return 65535
	}
}
