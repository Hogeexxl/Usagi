package storage

import (
	"context"
	"embed"
	"encoding/json"
	"fmt"
	"reflect"
	"sort"
	"sync"
)

//go:embed legacy_profiles/*.json
var legacyProfileFiles embed.FS

type legacyProfile struct {
	Version int                 `json:"version"`
	Tables  map[string][]string `json:"tables"`
}

type legacyProfileMatch struct {
	Version int
	Variant string
}

var loadLegacyProfiles = sync.OnceValues(func() (map[int]legacyProfile, error) {
	entries, err := legacyProfileFiles.ReadDir("legacy_profiles")
	if err != nil {
		return nil, newStorageError(ErrorInvalidState, err)
	}
	profiles := make(map[int]legacyProfile)
	for _, entry := range entries {
		data, err := legacyProfileFiles.ReadFile("legacy_profiles/" + entry.Name())
		if err != nil {
			return nil, newStorageError(ErrorInvalidState, err)
		}
		var profile legacyProfile
		if err := json.Unmarshal(data, &profile); err != nil {
			return nil, newStorageError(ErrorInvalidState, err)
		}
		if profile.Version < 1 || profile.Version > 14 || profile.Tables == nil {
			return nil, newStorageError(ErrorInvalidState, fmt.Errorf("invalid Legacy Profile %s", entry.Name()))
		}
		if _, exists := profiles[profile.Version]; exists {
			return nil, newStorageError(ErrorInvalidState, fmt.Errorf("duplicate Legacy Profile %d", profile.Version))
		}
		profiles[profile.Version] = profile
	}
	if len(profiles) != 14 {
		return nil, newStorageError(ErrorInvalidState, fmt.Errorf("expected 14 Legacy Profiles, got %d", len(profiles)))
	}
	return profiles, nil
})

func preflightLegacySource(ctx context.Context, path string) (legacyProfileMatch, error) {
	db, err := openInspection(path)
	if err != nil {
		return legacyProfileMatch{}, err
	}
	defer db.Close()
	if err := quickCheck(ctx, db); err != nil {
		return legacyProfileMatch{}, err
	}
	var version int
	if err := db.QueryRowContext(ctx, "PRAGMA user_version").Scan(&version); err != nil {
		return legacyProfileMatch{}, mapSQLiteError(err)
	}
	if version < 1 || version > 14 {
		return legacyProfileMatch{}, schemaMismatch("database", path, fmt.Errorf("not a supported Rust Legacy version: %d", version))
	}
	profiles, err := loadLegacyProfiles()
	if err != nil {
		return legacyProfileMatch{}, err
	}
	rows, err := db.QueryContext(ctx, "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT GLOB 'sqlite_*' ORDER BY name COLLATE BINARY")
	if err != nil {
		return legacyProfileMatch{}, mapSQLiteError(err)
	}
	defer rows.Close()
	var names []string
	for rows.Next() {
		var name string
		if err := rows.Scan(&name); err != nil {
			return legacyProfileMatch{}, mapSQLiteError(err)
		}
		names = append(names, name)
	}
	if err := rows.Err(); err != nil {
		return legacyProfileMatch{}, mapSQLiteError(err)
	}
	if err := rows.Close(); err != nil {
		return legacyProfileMatch{}, mapSQLiteError(err)
	}
	actual := make(map[string][]string, len(names))
	for _, name := range names {
		columns, err := inspectColumns(ctx, db, name)
		if err != nil {
			return legacyProfileMatch{}, err
		}
		for _, column := range columns {
			actual[name] = append(actual[name], column.name)
		}
		sort.Strings(actual[name])
	}
	profile := profiles[version]
	if reflect.DeepEqual(profile.Tables, actual) {
		return legacyProfileMatch{Version: version}, nil
	}
	if version <= 10 {
		variants := []struct {
			name   string
			absent []string
		}{
			{"app_meta_without_metadata_parser_version", []string{"metadata_parser_version"}},
			{"app_meta_without_last_full_import_completed_at_ms", []string{"last_full_import_completed_at_ms"}},
			{"app_meta_without_both_v11_assist_columns", []string{"metadata_parser_version", "last_full_import_completed_at_ms"}},
		}
		for _, variant := range variants {
			expected := make(map[string][]string, len(profile.Tables))
			for table, columns := range profile.Tables {
				for _, column := range columns {
					absent := false
					if table == "app_meta" {
						for _, name := range variant.absent {
							if column == name {
								absent = true
							}
						}
					}
					if !absent {
						expected[table] = append(expected[table], column)
					}
				}
			}
			if reflect.DeepEqual(expected, actual) {
				return legacyProfileMatch{Version: version, Variant: variant.name}, nil
			}
		}
	}
	return legacyProfileMatch{}, schemaMismatch("Legacy Profile", path, fmt.Errorf("tables or columns differ from Rust v%d", version))
}
