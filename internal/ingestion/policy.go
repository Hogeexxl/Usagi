package ingestion

import (
	"fmt"
	"time"
)

const (
	DefaultInterval = 300 * time.Second
	MinInterval     = 60 * time.Second
	MaxInterval     = 3600 * time.Second
)

type Config struct {
	Interval time.Duration
}

func DefaultConfig() Config {
	return Config{Interval: DefaultInterval}
}

func (c Config) Validate() error {
	if c.Interval < MinInterval || c.Interval > MaxInterval {
		return fmt.Errorf("scan interval must be between %s and %s", MinInterval, MaxInterval)
	}
	return nil
}
