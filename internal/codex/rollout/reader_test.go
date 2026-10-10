package rollout

import (
	"bytes"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/Hogeexxl/Usagi/internal/platform"
	"github.com/klauspost/compress/zstd"
	"github.com/zeebo/blake3"
)

func TestReaderPlainFixedViewCRLFAndHalfLine(t *testing.T) {
	path := filepath.Join(t.TempDir(), "rollout-reader.jsonl")
	initial := []byte("{}\r\n{\"pending\":")
	if err := os.WriteFile(path, initial, 0o600); err != nil {
		t.Fatal(err)
	}
	file, before := discoveredPhysicalFile(t, path, false)
	if err := os.WriteFile(path, append(append([]byte(nil), initial...), []byte("\"later\":true}\n")...), 0o600); err != nil {
		t.Fatal(err)
	}
	var got []Record
	result, err := Read(readPlan(file, int64(len(initial))), func(record Record) error {
		got = append(got, Record{
			SourceFileID: record.SourceFileID, Generation: record.Generation,
			LogicalStartOffset: record.LogicalStartOffset, LogicalEndOffset: record.LogicalEndOffset,
			JSON: append([]byte(nil), record.JSON...),
		})
		return nil
	}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 1 || string(got[0].JSON) != "{}" || got[0].LogicalStartOffset != 0 || got[0].LogicalEndOffset != 4 {
		t.Fatalf("records = %+v", got)
	}
	if result.PhysicalCommittedOffset != 4 || result.LogicalCommittedOffset != 4 || !result.UnverifiedTail || !result.FixedViewExhausted {
		t.Fatalf("Read result = %+v", result)
	}
	wantGuard := blake3.Sum256(initial[:4])
	if !bytes.Equal(result.GuardHash, wantGuard[:]) {
		t.Fatalf("guard = %x, want %x", result.GuardHash, wantGuard)
	}
	if before.Size != int64(len(initial)) {
		t.Fatalf("discovery size = %d, want %d", before.Size, len(initial))
	}
}

func TestReaderGuardContinuationAndMismatch(t *testing.T) {
	path := filepath.Join(t.TempDir(), "rollout-guard.jsonl")
	content := []byte("first\nsecond\n")
	if err := os.WriteFile(path, content, 0o600); err != nil {
		t.Fatal(err)
	}
	file, metadata := discoveredPhysicalFile(t, path, false)
	guard := blake3.Sum256(content[:6])
	plan := readPlan(file, metadata.Size)
	plan.PhysicalStartOffset = 6
	plan.ExpectedGuard = guard[:]
	var records []Record
	result, err := Read(plan, func(record Record) error {
		record.JSON = append([]byte(nil), record.JSON...)
		records = append(records, record)
		return nil
	}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if len(records) != 1 || string(records[0].JSON) != "second" || records[0].LogicalStartOffset != 6 || result.PhysicalCommittedOffset != int64(len(content)) {
		t.Fatalf("records/result = %+v / %+v", records, result)
	}
	plan.ExpectedGuard = make([]byte, len(blake3.Sum256(nil)))
	if _, err := Read(plan, nil, nil); !errors.Is(err, ErrGuardMismatch) {
		t.Fatalf("guard mismatch error = %v", err)
	}
}

func TestReaderMaximumLineBoundaryAndOversizedGapHasNoBody(t *testing.T) {
	path := filepath.Join(t.TempDir(), "rollout-maxline.jsonl")
	validLine := bytes.Repeat([]byte{'v'}, int(MaxRolloutLineBytes))
	oversizedLine := bytes.Repeat([]byte{'x'}, int(MaxRolloutLineBytes)+1)
	content := make([]byte, 0, len(validLine)+len(oversizedLine)+2)
	content = append(content, validLine...)
	content = append(content, '\n')
	content = append(content, oversizedLine...)
	content = append(content, '\n')
	if err := os.WriteFile(path, content, 0o600); err != nil {
		t.Fatal(err)
	}
	file, metadata := discoveredPhysicalFile(t, path, false)
	var recordLengths []int
	var gaps []Gap
	result, err := Read(readPlan(file, metadata.Size), func(record Record) error {
		recordLengths = append(recordLengths, len(record.JSON))
		return nil
	}, func(gap Gap) error {
		gaps = append(gaps, gap)
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if len(recordLengths) != 1 || int64(recordLengths[0]) != MaxRolloutLineBytes || len(gaps) != 1 || gaps[0].Kind != GapOversized {
		t.Fatalf("records=%v gaps=%+v", recordLengths, gaps)
	}
	if gaps[0].LogicalStartOffset != MaxRolloutLineBytes+1 || gaps[0].LogicalEndOffset != int64(len(content)) || result.GapCount != 1 {
		t.Fatalf("gap/result = %+v / %+v", gaps[0], result)
	}
}

func TestReaderSourceChangedDuringReadRejectsBatch(t *testing.T) {
	path := filepath.Join(t.TempDir(), "rollout-changing.jsonl")
	if err := os.WriteFile(path, []byte("x\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	beforeTime := time.Unix(1_700_000_000, 0)
	if err := os.Chtimes(path, beforeTime, beforeTime); err != nil {
		t.Fatal(err)
	}
	file, metadata := discoveredPhysicalFile(t, path, false)
	result, err := Read(readPlan(file, metadata.Size), func(Record) error {
		if err := os.WriteFile(path, []byte("y\n"), 0o600); err != nil {
			return err
		}
		changedTime := beforeTime.Add(2 * time.Second)
		return os.Chtimes(path, changedTime, changedTime)
	}, nil)
	if !errors.Is(err, ErrSourceChanged) {
		t.Fatalf("Read() error = %v, want source changed", err)
	}
	if result.PhysicalCommittedOffset != 0 || result.LogicalCommittedOffset != 0 || len(result.GuardHash) != 0 ||
		result.FixedViewExhausted || result.UnverifiedTail || result.RecordCount != 0 || result.GapCount != 0 {
		t.Fatalf("invalid batch returned result %+v", result)
	}
}

func TestReaderCompressedGuardDoesNotRequireLFBoundary(t *testing.T) {
	path := filepath.Join(t.TempDir(), "rollout-guard.jsonl.zst")
	content := []byte{0x28, 0xb5, 0x2f, 0xfd}
	if err := os.WriteFile(path, content, 0o600); err != nil {
		t.Fatal(err)
	}
	file, metadata := discoveredPhysicalFile(t, path, true)
	guard := blake3.Sum256(content)
	err := verifyGuard(GuardPlan{
		Path: path, Identity: file.Identity, PhysicalStartOffset: int64(len(content)),
		PhysicalObservedSize: metadata.Size, PhysicalObservedMTimeNS: metadata.MTimeNS,
		ExpectedGuard: guard[:], Compressed: true,
	})
	if err != nil {
		t.Fatalf("compressed boundary was rejected: %v", err)
	}
}

func TestZstdReaderUsesPhysicalAndLogicalCoordinatesAndKeepsHalfLine(t *testing.T) {
	fullPath := filepath.Join(t.TempDir(), "rollout-zstd.jsonl.zst")
	logical := []byte(fmt.Sprintf("{\"payload\":\"%s\"}\n", strings.Repeat("x", 128*1024)))
	compressed := encodeRolloutZstd(t, logical)
	if err := os.WriteFile(fullPath, compressed, 0o600); err != nil {
		t.Fatal(err)
	}
	file, metadata := discoveredPhysicalFile(t, fullPath, true)
	var recordJSON []byte
	result, err := Read(readPlan(file, metadata.Size), func(got Record) error {
		recordJSON = append([]byte(nil), got.JSON...)
		return nil
	}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if string(recordJSON) != string(bytes.TrimSuffix(logical, []byte{'\n'})) || result.RecordCount != 1 || result.UnverifiedTail {
		t.Fatalf("record/result = %q / %+v", recordJSON, result)
	}
	if result.PhysicalCommittedOffset != metadata.Size || result.PhysicalCommittedOffset > metadata.Size ||
		result.LogicalCommittedOffset != int64(len(logical)) || result.LogicalCommittedOffset <= result.PhysicalCommittedOffset {
		t.Fatalf("physical/logical coordinates = %+v (compressed=%d)", result, metadata.Size)
	}

	halfPath := filepath.Join(t.TempDir(), "rollout-half.jsonl.zst")
	halfLogical := []byte("{\"ok\":true}\n{\"half\":")
	if err := os.WriteFile(halfPath, encodeRolloutZstd(t, halfLogical), 0o600); err != nil {
		t.Fatal(err)
	}
	halfFile, halfMetadata := discoveredPhysicalFile(t, halfPath, true)
	result, err = Read(readPlan(halfFile, halfMetadata.Size), nil, nil)
	if err != nil {
		t.Fatal(err)
	}
	if !result.UnverifiedTail || result.PhysicalCommittedOffset != halfMetadata.Size ||
		result.LogicalCommittedOffset != int64(len("{\"ok\":true}\n")) || !result.FixedViewExhausted {
		t.Fatalf("half-line zstd result = %+v", result)
	}
}

func TestTwinStreamingEqualityPrefixAndLengthEdges(t *testing.T) {
	largePrefix := bytes.Repeat([]byte("{\"x\":1}\n"), 9000)
	tests := []struct {
		name       string
		plain      []byte
		decoded    []byte
		wantPrefix bool
		wantErr    bool
	}{
		{name: "short equal", plain: []byte("{}\n"), decoded: []byte("{}\n")},
		{name: "short complete prefix", plain: []byte("{}\n{\"next\":true}\n"), decoded: []byte("{}\n"), wantPrefix: true},
		{name: "empty complete prefix", plain: []byte("{}\n"), decoded: []byte{}, wantPrefix: true},
		{name: "compressed longer", plain: []byte("{}\n"), decoded: []byte("{}\nmore\n"), wantErr: true},
		{name: "different contents", plain: []byte("{\"a\":1}\n"), decoded: []byte("{\"b\":1}\n"), wantErr: true},
		{name: "cross buffer prefix", plain: append(append([]byte(nil), largePrefix...), []byte("{\"tail\":1}\n")...), decoded: largePrefix, wantPrefix: true},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			dir := t.TempDir()
			plainPath := filepath.Join(dir, "rollout-twin.jsonl")
			compressedPath := plainPath + ".zst"
			if err := os.WriteFile(plainPath, test.plain, 0o600); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(compressedPath, encodeRolloutZstd(t, test.decoded), 0o600); err != nil {
				t.Fatal(err)
			}
			plain, _ := discoveredPhysicalFile(t, plainPath, false)
			compressed, _ := discoveredPhysicalFile(t, compressedPath, true)
			proof, err := VerifyRedundantTwin(plain, compressed)
			if test.wantErr {
				if !errors.Is(err, ErrTwinMismatch) {
					t.Fatalf("VerifyRedundantTwin() error = %v, want mismatch", err)
				}
				return
			}
			if err != nil {
				t.Fatal(err)
			}
			if proof.CompressedIsPrefix != test.wantPrefix || proof.LogicalSize != int64(len(test.decoded)) {
				t.Fatalf("proof = %+v", proof)
			}
		})
	}
}

func discoveredPhysicalFile(t *testing.T, path string, compressed bool) (DiscoveredFile, platform.FileMetadata) {
	t.Helper()
	file, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	metadata, err := platform.MetadataFromFile(file)
	if err != nil {
		t.Fatal(err)
	}
	discovered := DiscoveredFile{
		Path: path, Area: AreaSessions,
		Identity: PhysicalIdentity{DeviceID: metadata.Identity.DeviceID, Inode: metadata.Identity.Inode},
		Size:     metadata.Size, MTimeNS: metadata.MTimeNS, Compressed: compressed,
	}
	return discovered, metadata
}

func readPlan(file DiscoveredFile, physicalSize int64) ReadPlan {
	return ReadPlan{
		SourceFileID: 1, Generation: 1, Path: file.Path, Identity: file.Identity,
		Compressed: file.Compressed, PhysicalObservedSize: physicalSize, PhysicalObservedMTimeNS: file.MTimeNS,
	}
}

func encodeRolloutZstd(t *testing.T, input []byte) []byte {
	t.Helper()
	encoder, err := zstd.NewWriter(nil, zstd.WithEncoderConcurrency(1))
	if err != nil {
		t.Fatal(err)
	}
	defer encoder.Close()
	return encoder.EncodeAll(input, nil)
}
