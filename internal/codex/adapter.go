package codex

import (
	"context"
	"errors"
	"time"

	"github.com/Hogeexxl/Usagi/internal/domain"
	"github.com/Hogeexxl/Usagi/internal/source"
)

type ConfigResolver interface {
	Resolve() ConfigResolution
}

type defaultConfigResolver struct{}

func (defaultConfigResolver) Resolve() ConfigResolution {
	config, err := ResolveDefaultConfig()
	return ConfigResolution{Config: config, Err: err}
}

type Adapter struct {
	descriptor source.Descriptor
	resolver   ConfigResolver
	clock      func() int64
}

func NewAdapter() (*Adapter, error) {
	return newAdapter(defaultConfigResolver{}, func() int64 { return time.Now().UnixMilli() })
}

func NewAdapterWithResolver(resolver ConfigResolver) (*Adapter, error) {
	return newAdapter(resolver, func() int64 { return time.Now().UnixMilli() })
}

func newAdapter(resolver ConfigResolver, clock func() int64) (*Adapter, error) {
	if resolver == nil || clock == nil {
		return nil, errors.New("Codex config resolver is nil")
	}
	descriptor, err := source.NewDescriptor(domain.SourceCodex, "Codex")
	if err != nil {
		return nil, err
	}
	return &Adapter{descriptor: descriptor, resolver: resolver, clock: clock}, nil
}

func (a *Adapter) Descriptor() source.Descriptor {
	return a.descriptor
}

func (a *Adapter) Availability(_ context.Context) (source.Availability, error) {
	resolution := a.resolver.Resolve()
	if resolution.Err != nil {
		code := "CODEX_HOME_RESOLUTION_FAILED"
		var configErr *ConfigError
		if errors.As(resolution.Err, &configErr) && configErr.Code != "" {
			code = configErr.Code
		}
		return source.Availability{}, source.NewAdapterErrorWithCode(code, "Codex configuration is invalid", resolution.Err)
	}
	return source.Available(), nil
}
