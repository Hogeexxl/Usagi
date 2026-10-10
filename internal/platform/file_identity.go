package platform

import (
	"fmt"
	"math"
	"time"
)

type FileIdentity struct {
	DeviceID int64
	Inode    int64
}

type FileMetadata struct {
	Identity FileIdentity
	Size     int64
	MTimeNS  int64
}

func unixTimeNS(value time.Time) (int64, error) {
	seconds := value.Unix()
	if seconds < 0 {
		return 0, fmt.Errorf("file modification time predates Unix epoch")
	}
	const nanosPerSecond = int64(1_000_000_000)
	maxSeconds := int64(math.MaxInt64) / nanosPerSecond
	nanoseconds := int64(value.Nanosecond())
	if seconds > maxSeconds || seconds == maxSeconds && nanoseconds > int64(math.MaxInt64)%nanosPerSecond {
		return 0, fmt.Errorf("file modification time overflows Unix nanoseconds")
	}
	return seconds*nanosPerSecond + nanoseconds, nil
}

func windowsTimeNS(fileTimeTicks uint64) (int64, error) {
	const (
		epochDeltaTicks = uint64(116_444_736_000_000_000)
		nanosPerTick    = uint64(100)
	)
	if fileTimeTicks < epochDeltaTicks {
		return 0, fmt.Errorf("file modification time predates Unix epoch")
	}
	ticksSinceEpoch := fileTimeTicks - epochDeltaTicks
	if ticksSinceEpoch > uint64(math.MaxInt64)/nanosPerTick {
		return 0, fmt.Errorf("file modification time overflows Unix nanoseconds")
	}
	return int64(ticksSinceEpoch * nanosPerTick), nil
}
