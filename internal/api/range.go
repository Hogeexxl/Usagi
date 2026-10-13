package api

import (
	"errors"
	"net/url"
	"os"
	"strings"
	"time"

	"github.com/Hogeexxl/Usagi/internal/query"
)

type ResolvedRange struct {
	Key      string
	Query    query.DateRange
	StartMS  int64
	EndMS    int64
	Timezone string
}

var ErrLocalTimeUnavailable = errors.New("api: local IANA time zone unavailable")

func ResolveRange(values url.Values) (ResolvedRange, error) {
	key, from, to, err := parseRangeParams(values)
	if err != nil {
		return ResolvedRange{}, err
	}
	name, location, err := LocalTimezone()
	if err != nil {
		return ResolvedRange{}, err
	}
	return resolveRange(key, from, to, time.Now(), name, location)
}

func ResolveRangeAt(values url.Values, now time.Time, timezone string) (ResolvedRange, error) {
	key, from, to, err := parseRangeParams(values)
	if err != nil {
		return ResolvedRange{}, err
	}
	location, err := time.LoadLocation(timezone)
	if err != nil {
		return ResolvedRange{}, ErrLocalTimeUnavailable
	}
	return resolveRange(key, from, to, now, timezone, location)
}

func parseRangeParams(values url.Values) (string, string, string, error) {
	key, err := rangeScalar(values, "range")
	if err != nil || key == "" {
		return "", "", "", query.ErrInvalidRange
	}
	from, err := rangeOptionalScalar(values, "from")
	if err != nil {
		return "", "", "", query.ErrInvalidRange
	}
	to, err := rangeOptionalScalar(values, "to")
	if err != nil {
		return "", "", "", query.ErrInvalidRange
	}
	switch key {
	case "today", "yesterday", "7d", "30d", "year":
		if from != "" || to != "" || hasParam(values, "from") || hasParam(values, "to") {
			return "", "", "", query.ErrInvalidRange
		}
	case "custom":
		if from == "" || to == "" {
			return "", "", "", query.ErrInvalidRange
		}
	default:
		return "", "", "", query.ErrInvalidRange
	}
	return key, from, to, nil
}

func rangeScalar(values url.Values, name string) (string, error) {
	items, ok := values[name]
	if !ok || len(items) != 1 {
		return "", query.ErrInvalidRange
	}
	return items[0], nil
}

func rangeOptionalScalar(values url.Values, name string) (string, error) {
	items, ok := values[name]
	if !ok {
		return "", nil
	}
	if len(items) != 1 {
		return "", query.ErrInvalidRange
	}
	return items[0], nil
}

func hasParam(values url.Values, name string) bool {
	_, ok := values[name]
	return ok
}

func resolveRange(key, from, to string, now time.Time, timezone string, location *time.Location) (ResolvedRange, error) {
	var startDate, endDate time.Time
	localNow := now.In(location)
	today := time.Date(localNow.Year(), localNow.Month(), localNow.Day(), 0, 0, 0, 0, time.UTC)
	switch key {
	case "today":
		startDate = today
		endDate = today.AddDate(0, 0, 1)
	case "yesterday":
		startDate = today.AddDate(0, 0, -1)
		endDate = today
	case "7d":
		startDate = today.AddDate(0, 0, -6)
		endDate = today.AddDate(0, 0, 1)
	case "30d":
		startDate = today.AddDate(0, 0, -29)
		endDate = today.AddDate(0, 0, 1)
	case "year":
		startDate = time.Date(localNow.Year(), time.January, 1, 0, 0, 0, 0, time.UTC)
		endDate = time.Date(localNow.Year()+1, time.January, 1, 0, 0, 0, 0, time.UTC)
	case "custom":
		startDate, _ = parseCivilDate(from)
		endDate, _ = parseCivilDate(to)
		if startDate.IsZero() || endDate.IsZero() || startDate.After(endDate) {
			return ResolvedRange{}, query.ErrInvalidRange
		}
		endDate = endDate.AddDate(0, 0, 1)
	default:
		return ResolvedRange{}, query.ErrInvalidRange
	}
	start, err := localMidnight(startDate, location)
	if err != nil {
		return ResolvedRange{}, err
	}
	end, err := localMidnight(endDate, location)
	if err != nil {
		return ResolvedRange{}, err
	}
	if start.After(end) {
		return ResolvedRange{}, query.ErrInvalidRange
	}
	return ResolvedRange{
		Key: key,
		Query: query.DateRange{
			From: civilDateString(startDate),
			To:   civilDateString(endDate.AddDate(0, 0, -1)),
		},
		StartMS: start.UnixMilli(), EndMS: end.UnixMilli(), Timezone: timezone,
	}, nil
}

func parseCivilDate(value string) (time.Time, error) {
	if len(value) != 10 || value[4] != '-' || value[7] != '-' {
		return time.Time{}, query.ErrInvalidRange
	}
	for index, r := range value {
		if index != 4 && index != 7 && (r < '0' || r > '9') {
			return time.Time{}, query.ErrInvalidRange
		}
	}
	date, err := time.Parse(time.DateOnly, value)
	if err != nil || date.Format(time.DateOnly) != value {
		return time.Time{}, query.ErrInvalidRange
	}
	return time.Date(date.Year(), date.Month(), date.Day(), 0, 0, 0, 0, time.UTC), nil
}

func civilDateString(date time.Time) string {
	return date.Format(time.DateOnly)
}

func localMidnight(date time.Time, location *time.Location) (time.Time, error) {
	wall := time.Date(date.Year(), date.Month(), date.Day(), 0, 0, 0, 0, time.UTC)
	wallSeconds := wall.Unix()
	const radius = 48 * time.Hour
	offsets := make(map[int]struct{})
	for seconds := int64(-radius.Seconds()); seconds <= int64(radius.Seconds()); seconds += int64((15 * time.Minute).Seconds()) {
		_, offset := time.Unix(wallSeconds+seconds, 0).In(location).Zone()
		offsets[offset] = struct{}{}
	}
	var candidates []time.Time
	for offset := range offsets {
		candidate := time.Unix(wallSeconds-int64(offset), 0).UTC()
		local := candidate.In(location)
		if local.Year() == date.Year() && local.Month() == date.Month() && local.Day() == date.Day() &&
			local.Hour() == 0 && local.Minute() == 0 && local.Second() == 0 {
			candidates = append(candidates, candidate)
		}
	}
	if len(candidates) > 0 {
		return minTime(candidates), nil
	}

	start, end := wallSeconds-int64(radius.Seconds()), wallSeconds+int64(radius.Seconds())
	previousUTC := start
	_, previousOffset := time.Unix(previousUTC, 0).In(location).Zone()
	for utc := start + 60; utc <= end; utc += 60 {
		_, offset := time.Unix(utc, 0).In(location).Zone()
		if offset > previousOffset {
			transition := firstUTCWithOffset(previousUTC, utc, offset, location)
			beforeLocal := transition + int64(previousOffset)
			afterLocal := transition + int64(offset)
			if beforeLocal <= wallSeconds && wallSeconds < afterLocal {
				return time.Unix(transition, 0).UTC(), nil
			}
		}
		previousUTC, previousOffset = utc, offset
	}
	return time.Time{}, ErrLocalTimeUnavailable
}

func firstUTCWithOffset(low, high int64, offset int, location *time.Location) int64 {
	for low+1 < high {
		middle := low + (high-low)/2
		_, got := time.Unix(middle, 0).In(location).Zone()
		if got == offset {
			high = middle
		} else {
			low = middle
		}
	}
	return high
}

func minTime(values []time.Time) time.Time {
	result := values[0]
	for _, value := range values[1:] {
		if value.Before(result) {
			result = value
		}
	}
	return result
}

func LocalTimezone() (string, *time.Location, error) {
	if name, ok := os.LookupEnv("TZ"); ok {
		location, err := loadSystemTimezone(name)
		return name, location, err
	}
	name, err := systemTimezoneFromOS()
	if err != nil {
		return "", nil, ErrLocalTimeUnavailable
	}
	location, err := loadSystemTimezone(name)
	return name, location, err
}

func validTimezoneName(name string) bool {
	if name == "" || strings.HasPrefix(name, "/") {
		return false
	}
	for _, part := range strings.Split(name, "/") {
		if part == "" || part == "." || part == ".." {
			return false
		}
		for _, r := range part {
			if !(r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || strings.ContainsRune("_+-", r)) {
				return false
			}
		}
	}
	return true
}

func loadSystemTimezone(name string) (*time.Location, error) {
	if !validTimezoneName(name) {
		return nil, ErrLocalTimeUnavailable
	}
	location, err := time.LoadLocation(name)
	if err != nil {
		return nil, ErrLocalTimeUnavailable
	}
	return location, nil
}
