package source

import (
	"context"
	"fmt"
	"strings"
	"unicode"

	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

type RunContext struct {
	scanID  string
	source  domain.SourceID
	storage *Storage
}

func (r RunContext) ScanID() string {
	return r.scanID
}

func (r RunContext) Source() domain.SourceID {
	return r.source
}

func (r RunContext) Storage() *Storage {
	return r.storage
}

type StorageFactory struct {
	db *storage.DB
}

func NewStorageFactory(db *storage.DB) *StorageFactory {
	return &StorageFactory{db: db}
}

func (f *StorageFactory) Context(
	workerCtx context.Context,
	scanID string,
	descriptor Descriptor,
) (RunContext, error) {
	if f == nil || f.db == nil {
		return RunContext{}, fmt.Errorf("source storage factory has no database")
	}
	if workerCtx == nil {
		return RunContext{}, fmt.Errorf("source worker context is nil")
	}
	if strings.TrimSpace(scanID) == "" {
		return RunContext{}, fmt.Errorf("scan id must not be empty")
	}
	for _, r := range scanID {
		if unicode.IsControl(r) {
			return RunContext{}, fmt.Errorf("scan id must not contain control characters")
		}
	}
	if err := descriptor.Validate(); err != nil {
		return RunContext{}, err
	}
	storageCtx := context.WithoutCancel(workerCtx)
	boundStorage := &Storage{
		db:         f.db,
		scanID:     scanID,
		source:     descriptor.ID,
		storageCtx: storageCtx,
	}
	return RunContext{
		scanID:  scanID,
		source:  descriptor.ID,
		storage: boundStorage,
	}, nil
}
