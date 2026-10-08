package source

import (
	"fmt"
	"reflect"
	"sort"

	"github.com/Hogeexxl/Usagi/internal/domain"
)

type registryEntry struct {
	descriptor Descriptor
	adapter    Adapter
}

type Registry struct {
	entries map[domain.SourceID]registryEntry
	ordered []registryEntry
}

func NewRegistry(adapters ...Adapter) (*Registry, error) {
	entries := make(map[domain.SourceID]registryEntry, len(adapters))
	ordered := make([]registryEntry, 0, len(adapters))
	for _, adapter := range adapters {
		if isNilAdapter(adapter) {
			return nil, fmt.Errorf("source adapter is nil")
		}
		descriptor := adapter.Descriptor()
		if err := descriptor.Validate(); err != nil {
			return nil, err
		}
		if _, exists := entries[descriptor.ID]; exists {
			return nil, fmt.Errorf("source already registered: %s", descriptor.ID)
		}
		entry := registryEntry{descriptor: descriptor, adapter: adapter}
		entries[descriptor.ID] = entry
		ordered = append(ordered, entry)
	}
	sort.Slice(ordered, func(i, j int) bool {
		return ordered[i].descriptor.ID < ordered[j].descriptor.ID
	})
	return &Registry{entries: entries, ordered: ordered}, nil
}

func isNilAdapter(adapter Adapter) bool {
	if adapter == nil {
		return true
	}
	value := reflect.ValueOf(adapter)
	switch value.Kind() {
	case reflect.Ptr, reflect.Map, reflect.Slice, reflect.Func, reflect.Chan, reflect.Interface:
		return value.IsNil()
	default:
		return false
	}
}

func (r *Registry) Get(id domain.SourceID) (Adapter, bool) {
	entry, ok := r.entries[id]
	if !ok {
		return nil, false
	}
	return entry.adapter, true
}

func (r *Registry) Descriptor(id domain.SourceID) (Descriptor, bool) {
	entry, ok := r.entries[id]
	if !ok {
		return Descriptor{}, false
	}
	return entry.descriptor, true
}

func (r *Registry) SourceIDs() []domain.SourceID {
	ids := make([]domain.SourceID, len(r.ordered))
	for i, entry := range r.ordered {
		ids[i] = entry.descriptor.ID
	}
	return ids
}
