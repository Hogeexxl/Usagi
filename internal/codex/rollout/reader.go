package rollout

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"

	"github.com/Hogeexxl/Usagi/internal/platform"
	"github.com/klauspost/compress/zstd"
	"github.com/zeebo/blake3"
)

const readBufferBytes = 64 * 1024

var (
	ErrSourceChanged       = errors.New("rollout source changed during read")
	ErrSourceSymlink       = errors.New("rollout source symlink is not allowed")
	ErrInvalidReadPlan     = errors.New("invalid rollout read plan")
	ErrTwinMismatch        = errors.New("rollout plain and zstd files are not redundant twins")
	ErrIncompleteZstdInput = errors.New("zstd decoder did not consume the fixed physical view")
)

type GuardPlan struct {
	Path                    string
	Identity                PhysicalIdentity
	PhysicalStartOffset     int64
	PhysicalObservedSize    int64
	PhysicalObservedMTimeNS int64
	ExpectedGuard           []byte
	Compressed              bool
}

type ReadPlan struct {
	SourceFileID            int64
	Generation              int64
	Path                    string
	Identity                PhysicalIdentity
	Compressed              bool
	PhysicalStartOffset     int64
	PhysicalObservedSize    int64
	PhysicalObservedMTimeNS int64
	ExpectedGuard           []byte
}

type ReadResult struct {
	PhysicalCommittedOffset int64
	LogicalCommittedOffset  int64
	GuardHash               []byte
	FixedViewExhausted      bool
	UnverifiedTail          bool
	RecordCount             int64
	GapCount                int64
}

type RedundancyProof struct {
	PlainPath          string
	CompressedPath     string
	PlainIdentity      PhysicalIdentity
	CompressedIdentity PhysicalIdentity
	PlainSize          int64
	PlainMTimeNS       int64
	CompressedSize     int64
	CompressedMTimeNS  int64
	LogicalSize        int64
	CompressedIsPrefix bool
}

func verifyGuard(plan GuardPlan) error {
	if plan.Path == "" || plan.PhysicalStartOffset < 0 || plan.PhysicalObservedSize < 0 ||
		plan.PhysicalStartOffset > plan.PhysicalObservedSize || plan.Identity.DeviceID < 0 || plan.Identity.Inode < 0 {
		return fmt.Errorf("%w: invalid guard coordinates", ErrInvalidReadPlan)
	}
	file, before, err := openPhysicalFile(plan.Path, plan.Identity)
	if err != nil {
		return err
	}
	defer file.Close()
	if err := checkPhysicalView(before, before, plan.PhysicalObservedSize, plan.PhysicalObservedMTimeNS, plan.Compressed); err != nil {
		return err
	}
	if plan.PhysicalStartOffset == 0 {
		if len(plan.ExpectedGuard) != 0 {
			return ErrGuardMismatch
		}
	} else {
		var previous [1]byte
		if !plan.Compressed {
			if _, err := file.ReadAt(previous[:], plan.PhysicalStartOffset-1); err != nil {
				return ErrGuardMismatch
			}
			if previous[0] != '\n' {
				return ErrGuardMismatch
			}
		}
		if len(plan.ExpectedGuard) != len(blake3.Sum256(nil)) {
			return ErrGuardMismatch
		}
		guard, err := physicalGuard(file, plan.PhysicalStartOffset)
		if err != nil {
			return err
		}
		if !bytes.Equal(guard, plan.ExpectedGuard) {
			return ErrGuardMismatch
		}
	}
	after, err := platform.MetadataFromFile(file)
	if err != nil {
		return err
	}
	if err := checkPhysicalView(after, before, plan.PhysicalObservedSize, plan.PhysicalObservedMTimeNS, plan.Compressed); err != nil {
		return err
	}
	return verifyPathIdentity(plan.Path, plan.Identity)
}

func Read(plan ReadPlan, onRecord func(Record) error, onGap func(Gap) error) (ReadResult, error) {
	if plan.SourceFileID <= 0 || plan.Generation <= 0 || plan.Path == "" || plan.Identity.DeviceID < 0 || plan.Identity.Inode < 0 ||
		plan.PhysicalStartOffset < 0 || plan.PhysicalObservedSize < 0 || plan.PhysicalObservedMTimeNS < 0 ||
		plan.PhysicalStartOffset > plan.PhysicalObservedSize {
		return ReadResult{}, ErrInvalidReadPlan
	}
	if plan.Compressed && (plan.PhysicalStartOffset != 0 || len(plan.ExpectedGuard) != 0) {
		return ReadResult{}, fmt.Errorf("%w: zstd requires a full physical read", ErrInvalidReadPlan)
	}
	file, before, err := openPhysicalFile(plan.Path, plan.Identity)
	if err != nil {
		return ReadResult{}, err
	}
	defer file.Close()
	if err := checkPhysicalView(before, before, plan.PhysicalObservedSize, plan.PhysicalObservedMTimeNS, plan.Compressed); err != nil {
		return ReadResult{}, err
	}
	if !plan.Compressed {
		if err := verifyGuard(GuardPlan{
			Path: plan.Path, Identity: plan.Identity,
			PhysicalStartOffset: plan.PhysicalStartOffset, PhysicalObservedSize: plan.PhysicalObservedSize,
			PhysicalObservedMTimeNS: plan.PhysicalObservedMTimeNS,
			ExpectedGuard:           plan.ExpectedGuard,
		}); err != nil {
			return ReadResult{}, err
		}
	}

	result := ReadResult{}
	var logicalStart int64
	var logicalCommitted int64
	if !plan.Compressed {
		logicalStart = plan.PhysicalStartOffset
		logicalCommitted = logicalStart
		reader := io.NewSectionReader(file, plan.PhysicalStartOffset, plan.PhysicalObservedSize-plan.PhysicalStartOffset)
		logicalCommitted, result.UnverifiedTail, result.RecordCount, result.GapCount, err = streamJSONL(
			reader, logicalStart, plan.SourceFileID, plan.Generation, onRecord, onGap,
		)
		if err != nil {
			return ReadResult{}, err
		}
		result.PhysicalCommittedOffset = logicalCommitted
		result.FixedViewExhausted = true
	} else {
		section := io.NewSectionReader(file, 0, plan.PhysicalObservedSize)
		physical := &countingReader{reader: section}
		decoder, err := zstd.NewReader(physical, zstd.WithDecoderConcurrency(1))
		if err != nil {
			return ReadResult{}, fmt.Errorf("decode zstd rollout: %w", err)
		}
		logicalCommitted, result.UnverifiedTail, result.RecordCount, result.GapCount, err = streamJSONL(
			decoder, 0, plan.SourceFileID, plan.Generation, onRecord, onGap,
		)
		decoder.Close()
		if err != nil {
			return ReadResult{}, fmt.Errorf("decode zstd rollout: %w", err)
		}
		if physical.count != plan.PhysicalObservedSize {
			return ReadResult{}, fmt.Errorf("%w: read %d of %d bytes", ErrIncompleteZstdInput, physical.count, plan.PhysicalObservedSize)
		}
		result.PhysicalCommittedOffset = plan.PhysicalObservedSize
		result.FixedViewExhausted = true
	}
	result.LogicalCommittedOffset = logicalCommitted
	guard, err := physicalGuard(file, result.PhysicalCommittedOffset)
	if err != nil {
		return ReadResult{}, err
	}
	result.GuardHash = guard

	after, err := platform.MetadataFromFile(file)
	if err != nil {
		return ReadResult{}, err
	}
	if err := checkPhysicalView(after, before, plan.PhysicalObservedSize, plan.PhysicalObservedMTimeNS, plan.Compressed); err != nil {
		return ReadResult{}, err
	}
	if err := verifyPathIdentity(plan.Path, plan.Identity); err != nil {
		return ReadResult{}, err
	}
	return result, nil
}

func VerifyRedundantTwin(plain, compressed DiscoveredFile) (RedundancyProof, error) {
	if plain.Compressed || !compressed.Compressed || !isTwinCandidate(plain, compressed) ||
		plain.Size < 0 || plain.MTimeNS < 0 || compressed.Size < 0 || compressed.MTimeNS < 0 ||
		plain.Identity.DeviceID < 0 || plain.Identity.Inode < 0 || compressed.Identity.DeviceID < 0 || compressed.Identity.Inode < 0 {
		return RedundancyProof{}, fmt.Errorf("%w: invalid twin candidates", ErrInvalidReadPlan)
	}
	plainFile, plainBefore, err := openPhysicalFile(plain.Path, plain.Identity)
	if err != nil {
		return RedundancyProof{}, err
	}
	defer plainFile.Close()
	compressedFile, compressedBefore, err := openPhysicalFile(compressed.Path, compressed.Identity)
	if err != nil {
		return RedundancyProof{}, err
	}
	defer compressedFile.Close()
	if plainBefore.Size != plain.Size || plainBefore.MTimeNS != plain.MTimeNS ||
		compressedBefore.Size != compressed.Size || compressedBefore.MTimeNS != compressed.MTimeNS {
		return RedundancyProof{}, ErrSourceChanged
	}

	physical := &countingReader{reader: io.NewSectionReader(compressedFile, 0, compressed.Size)}
	decoder, err := zstd.NewReader(physical, zstd.WithDecoderConcurrency(1))
	if err != nil {
		return RedundancyProof{}, fmt.Errorf("decode zstd twin: %w", err)
	}
	plainReader := io.NewSectionReader(plainFile, 0, plain.Size)
	logicalSize, isPrefix, lastByte, err := compareTwinStreams(plainReader, decoder, plain.Size)
	decoder.Close()
	if err != nil {
		return RedundancyProof{}, err
	}
	if physical.count != compressed.Size {
		return RedundancyProof{}, fmt.Errorf("%w: twin decoder read %d of %d bytes", ErrIncompleteZstdInput, physical.count, compressed.Size)
	}
	if isPrefix && logicalSize > 0 && logicalSize < plain.Size && lastByte != '\n' {
		return RedundancyProof{}, ErrTwinMismatch
	}
	plainAfter, err := platform.MetadataFromFile(plainFile)
	if err != nil {
		return RedundancyProof{}, err
	}
	compressedAfter, err := platform.MetadataFromFile(compressedFile)
	if err != nil {
		return RedundancyProof{}, err
	}
	if plainAfter != plainBefore || compressedAfter != compressedBefore {
		return RedundancyProof{}, ErrSourceChanged
	}
	if err := verifyPathIdentity(plain.Path, plain.Identity); err != nil {
		return RedundancyProof{}, err
	}
	if err := verifyPathIdentity(compressed.Path, compressed.Identity); err != nil {
		return RedundancyProof{}, err
	}
	return RedundancyProof{
		PlainPath: filepath.Clean(plain.Path), CompressedPath: filepath.Clean(compressed.Path),
		PlainIdentity: plain.Identity, CompressedIdentity: compressed.Identity,
		PlainSize: plain.Size, PlainMTimeNS: plain.MTimeNS,
		CompressedSize: compressed.Size, CompressedMTimeNS: compressed.MTimeNS,
		LogicalSize:        logicalSize,
		CompressedIsPrefix: isPrefix && logicalSize < plain.Size,
	}, nil
}

func isTwinCandidate(plain, compressed DiscoveredFile) bool {
	if !strings.HasSuffix(plain.Path, ".jsonl") || !strings.HasSuffix(compressed.Path, ".jsonl.zst") {
		return false
	}
	return filepath.Clean(plain.Path) == filepath.Clean(strings.TrimSuffix(compressed.Path, ".zst"))
}

func compareTwinStreams(plain, compressed io.Reader, plainSize int64) (logicalSize int64, prefix bool, lastByte byte, err error) {
	plainBuffer := make([]byte, readBufferBytes)
	compressedBuffer := make([]byte, readBufferBytes)
	var plainSizeSeen, compressedSizeSeen int64
	plainDone, compressedDone := false, false
	for !plainDone || !compressedDone {
		plainN, plainErr := 0, io.EOF
		if !plainDone {
			plainN, plainErr = io.ReadFull(plain, plainBuffer)
		}
		compressedN, compressedErr := 0, io.EOF
		if !compressedDone {
			compressedN, compressedErr = io.ReadFull(compressed, compressedBuffer)
		}
		if plainErr != nil && plainErr != io.EOF && plainErr != io.ErrUnexpectedEOF {
			return 0, false, 0, plainErr
		}
		if compressedErr != nil && compressedErr != io.EOF && compressedErr != io.ErrUnexpectedEOF {
			return 0, false, 0, fmt.Errorf("decode zstd twin: %w", compressedErr)
		}
		common := plainN
		if compressedN < common {
			common = compressedN
		}
		if !bytes.Equal(plainBuffer[:common], compressedBuffer[:common]) {
			return 0, false, 0, ErrTwinMismatch
		}
		if plainN > 0 {
			plainSizeSeen += int64(plainN)
		}
		if compressedN > 0 {
			compressedSizeSeen += int64(compressedN)
			lastByte = compressedBuffer[compressedN-1]
		}
		plainDone = plainDone || plainErr != nil
		compressedDone = compressedDone || compressedErr != nil
	}
	if plainSizeSeen != plainSize || compressedSizeSeen > plainSizeSeen {
		return 0, false, 0, ErrTwinMismatch
	}
	return compressedSizeSeen, compressedSizeSeen < plainSizeSeen, lastByte, nil
}

func streamJSONL(reader io.Reader, logicalStart int64, sourceFileID, generation int64, onRecord func(Record) error, onGap func(Gap) error) (committed int64, halfLine bool, records, gaps int64, err error) {
	buffer := make([]byte, readBufferBytes)
	line := make([]byte, 0, 4096)
	lineStart, logicalOffset, lineLength := logicalStart, logicalStart, 0
	committed = logicalStart
	oversized := false
	noProgress := 0
	for {
		n, readErr := reader.Read(buffer)
		if n == 0 && readErr == nil {
			noProgress++
			if noProgress == 100 {
				return committed, false, records, gaps, io.ErrNoProgress
			}
			continue
		}
		noProgress = 0
		for _, value := range buffer[:n] {
			if value == '\n' {
				end := logicalOffset + 1
				if oversized {
					if onGap != nil {
						if err := onGap(Gap{SourceFileID: sourceFileID, Generation: generation, LogicalStartOffset: lineStart, LogicalEndOffset: end, Kind: GapOversized}); err != nil {
							return committed, false, records, gaps, err
						}
					}
					gaps++
				} else {
					json := line
					if len(json) > 0 && json[len(json)-1] == '\r' {
						json = json[:len(json)-1]
					}
					if onRecord != nil {
						if err := onRecord(Record{SourceFileID: sourceFileID, Generation: generation, LogicalStartOffset: lineStart, LogicalEndOffset: end, JSON: json}); err != nil {
							return committed, false, records, gaps, err
						}
					}
					records++
				}
				lineStart, committed, lineLength, line, oversized = end, end, 0, line[:0], false
				logicalOffset = end
				continue
			}
			logicalOffset++
			lineLength++
			if !oversized {
				if int64(lineLength) > MaxRolloutLineBytes {
					line = line[:0]
					oversized = true
				} else {
					line = append(line, value)
				}
			}
		}
		if readErr != nil {
			if readErr != io.EOF {
				return committed, false, records, gaps, readErr
			}
			return committed, lineLength > 0, records, gaps, nil
		}
	}
}

func physicalGuard(file *os.File, offset int64) ([]byte, error) {
	if offset < 0 {
		return nil, ErrInvalidReadPlan
	}
	if offset == 0 {
		return nil, nil
	}
	start := int64(0)
	if offset > GuardWindowBytes {
		start = offset - GuardWindowBytes
	}
	body := make([]byte, int(offset-start))
	if n, err := file.ReadAt(body, start); err != nil && !(err == io.EOF && n == len(body)) {
		return nil, err
	}
	hash := blake3.Sum256(body)
	return hash[:], nil
}

func openPhysicalFile(path string, identity PhysicalIdentity) (*os.File, platform.FileMetadata, error) {
	info, err := os.Lstat(path)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, platform.FileMetadata{}, fmt.Errorf("%w: path disappeared", ErrSourceChanged)
		}
		return nil, platform.FileMetadata{}, err
	}
	if info.Mode()&os.ModeSymlink != 0 {
		return nil, platform.FileMetadata{}, ErrSourceSymlink
	}
	if !info.Mode().IsRegular() {
		return nil, platform.FileMetadata{}, fmt.Errorf("%w: source is not a regular file", ErrSourceChanged)
	}
	file, err := os.Open(path)
	if err != nil {
		return nil, platform.FileMetadata{}, err
	}
	metadata, err := platform.MetadataFromFile(file)
	if err != nil {
		file.Close()
		return nil, platform.FileMetadata{}, err
	}
	want := platform.FileIdentity{DeviceID: identity.DeviceID, Inode: identity.Inode}
	if metadata.Identity != want {
		file.Close()
		return nil, platform.FileMetadata{}, ErrSourceChanged
	}
	return file, metadata, nil
}

func verifyPathIdentity(path string, identity PhysicalIdentity) error {
	file, _, err := openPhysicalFile(path, identity)
	if err != nil {
		return err
	}
	return file.Close()
}

func checkPhysicalView(current, before platform.FileMetadata, observedSize, observedMTimeNS int64, compressed bool) error {
	wantIdentity := platform.FileIdentity{DeviceID: before.Identity.DeviceID, Inode: before.Identity.Inode}
	if current.Identity != wantIdentity || current.Size < observedSize {
		return ErrSourceChanged
	}
	if compressed {
		if current.Size != observedSize || current.MTimeNS != observedMTimeNS {
			return ErrSourceChanged
		}
		return nil
	}
	if current.Size == observedSize && current.MTimeNS != observedMTimeNS {
		return ErrSourceChanged
	}
	if current.Size == before.Size && current.MTimeNS != before.MTimeNS {
		return ErrSourceChanged
	}
	return nil
}

type countingReader struct {
	reader io.Reader
	count  int64
}

func (reader *countingReader) Read(buffer []byte) (int, error) {
	n, err := reader.reader.Read(buffer)
	reader.count += int64(n)
	return n, err
}
