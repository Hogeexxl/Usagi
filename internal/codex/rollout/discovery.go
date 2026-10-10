package rollout

import (
	"errors"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"

	"github.com/Hogeexxl/Usagi/internal/platform"
	"github.com/google/uuid"
)

var filenameUUID = regexp.MustCompile(`(?i)[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}`)
var numberedSegment = regexp.MustCompile(`_[0-9]+$`)

func Discover(home string, startedAtMS int64) DiscoverySnapshot {
	if startedAtMS < 0 {
		startedAtMS = 0
	}
	normalizedHome, err := filepath.Abs(home)
	if err != nil {
		return DiscoverySnapshot{StartedAtMS: startedAtMS, Sessions: RegionUnavailable, Archived: RegionUnavailable}
	}
	normalizedHome = filepath.Clean(normalizedHome)
	sessions, sessionFiles := walkArea(filepath.Join(normalizedHome, "sessions"), AreaSessions)
	archived, archivedFiles := walkArea(filepath.Join(normalizedHome, "archived_sessions"), AreaArchived)
	files := deduplicateFiles(append(sessionFiles, archivedFiles...))
	sort.Slice(files, func(i, j int) bool { return discoveredLess(files[i], files[j]) })
	return DiscoverySnapshot{StartedAtMS: startedAtMS, Sessions: sessions, Archived: archived, Files: files}
}

func walkArea(root string, area Area) (RegionState, []DiscoveredFile) {
	rootInfo, err := os.Lstat(root)
	if errors.Is(err, os.ErrNotExist) {
		return RegionComplete, nil
	}
	if err != nil || rootInfo.Mode()&os.ModeSymlink != 0 || !rootInfo.IsDir() {
		return RegionUnavailable, nil
	}

	complete := true
	files := make([]DiscoveredFile, 0)
	err = filepath.WalkDir(root, func(path string, entry fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			complete = false
			return nil
		}
		if path == root {
			return nil
		}
		if entry.Type()&os.ModeSymlink != 0 {
			if entry.IsDir() {
				return filepath.SkipDir
			}
			return nil
		}
		if entry.IsDir() {
			return nil
		}
		if !isRolloutFilename(path) {
			return nil
		}
		info, err := entry.Info()
		if err != nil {
			complete = false
			return nil
		}
		if !info.Mode().IsRegular() {
			return nil
		}
		file, err := os.Open(path)
		if err != nil {
			complete = false
			return nil
		}
		metadata, metadataErr := platform.MetadataFromFile(file)
		closeErr := file.Close()
		if metadataErr != nil || closeErr != nil {
			complete = false
			return nil
		}
		pathInfo, err := os.Lstat(path)
		if err != nil || pathInfo.Mode()&os.ModeSymlink != 0 || !pathInfo.Mode().IsRegular() {
			complete = false
			return nil
		}
		pathFile, err := os.Open(path)
		if err != nil {
			complete = false
			return nil
		}
		pathMetadata, pathErr := platform.MetadataFromFile(pathFile)
		_ = pathFile.Close()
		if pathErr != nil || pathMetadata.Identity != metadata.Identity {
			complete = false
			return nil
		}
		normalizedPath, err := filepath.Abs(path)
		if err != nil {
			complete = false
			return nil
		}
		files = append(files, DiscoveredFile{
			Path:              filepath.Clean(normalizedPath),
			Area:              area,
			Identity:          PhysicalIdentity{DeviceID: metadata.Identity.DeviceID, Inode: metadata.Identity.Inode},
			Size:              metadata.Size,
			MTimeNS:           metadata.MTimeNS,
			ThreadIDCandidate: filenameThreadID(path),
			Compressed:        strings.HasSuffix(path, ".jsonl.zst"),
		})
		return nil
	})
	if err != nil {
		complete = false
	}
	if complete {
		return RegionComplete, files
	}
	return RegionUnavailable, files
}

func isRolloutFilename(path string) bool {
	name := filepath.Base(path)
	if !strings.HasPrefix(name, "rollout-") {
		return false
	}
	stem := strings.TrimPrefix(name, "rollout-")
	if strings.HasSuffix(stem, ".jsonl.zst") {
		stem = strings.TrimSuffix(stem, ".jsonl.zst")
	} else if strings.HasSuffix(stem, ".jsonl") {
		stem = strings.TrimSuffix(stem, ".jsonl")
	} else {
		return false
	}
	return stem != ""
}

func filenameThreadID(path string) string {
	name := filepath.Base(path)
	stem := strings.TrimPrefix(name, "rollout-")
	stem = strings.TrimSuffix(strings.TrimSuffix(stem, ".zst"), ".jsonl")
	stem = numberedSegment.ReplaceAllString(stem, "")
	matches := filenameUUID.FindAllString(stem, -1)
	if len(matches) == 0 {
		return ""
	}
	parsed, err := uuid.Parse(matches[len(matches)-1])
	if err != nil {
		return ""
	}
	return parsed.String()
}

func deduplicateFiles(files []DiscoveredFile) []DiscoveredFile {
	winners := make(map[PhysicalIdentity]int, len(files))
	keep := make([]bool, len(files))
	for i := range keep {
		keep[i] = true
	}
	for index := range files {
		identity := files[index].Identity
		previous, exists := winners[identity]
		if !exists {
			winners[identity] = index
			continue
		}
		if discoveredAliasLess(files[index], files[previous]) {
			keep[previous] = false
			winners[identity] = index
		} else {
			keep[index] = false
		}
	}
	unique := make([]DiscoveredFile, 0, len(winners))
	for index, file := range files {
		if keep[index] {
			unique = append(unique, file)
		}
	}
	return unique
}

func discoveredAliasLess(left, right DiscoveredFile) bool {
	if left.Area != right.Area {
		return left.Area == AreaSessions
	}
	return left.Path < right.Path
}

func discoveredLess(left, right DiscoveredFile) bool {
	if left.Area != right.Area {
		return left.Area < right.Area
	}
	if left.Path != right.Path {
		return left.Path < right.Path
	}
	if left.Identity.DeviceID != right.Identity.DeviceID {
		return left.Identity.DeviceID < right.Identity.DeviceID
	}
	return left.Identity.Inode < right.Identity.Inode
}
