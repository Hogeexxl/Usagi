package platform

import (
	"os"
	"path/filepath"
	"runtime"
	"testing"
	"time"
)

func TestFileIdentityTracksOpenFileAcrossRenameAndReplacement(t *testing.T) {
	directory := t.TempDir()
	path := filepath.Join(directory, "rollout.jsonl")
	renamedPath := filepath.Join(directory, "renamed.jsonl")
	if err := os.WriteFile(path, []byte("first"), 0o600); err != nil {
		t.Fatal(err)
	}

	file, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()
	before, err := IdentityFromFile(file)
	if err != nil {
		t.Fatal(err)
	}
	again, err := IdentityFromFile(file)
	if err != nil {
		t.Fatal(err)
	}
	if again != before {
		t.Fatalf("identity from same handle = %+v, want %+v", again, before)
	}
	samePath, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	samePathIdentity, identityErr := IdentityFromFile(samePath)
	closeErr := samePath.Close()
	if identityErr != nil {
		t.Fatal(identityErr)
	}
	if closeErr != nil {
		t.Fatal(closeErr)
	}
	if samePathIdentity != before {
		t.Fatalf("identity from reopened path = %+v, want %+v", samePathIdentity, before)
	}
	if before.DeviceID < 0 || before.Inode < 0 {
		t.Fatalf("identity = %+v, want non-negative values", before)
	}
	if runtime.GOOS == "windows" && before.DeviceID == 0 && before.Inode == 0 {
		t.Fatal("Windows identity is (0,0)")
	}

	if err := os.Rename(path, renamedPath); err != nil {
		t.Fatal(err)
	}
	afterRename, err := IdentityFromFile(file)
	if err != nil {
		t.Fatal(err)
	}
	if afterRename != before {
		t.Fatalf("identity after rename = %+v, want %+v", afterRename, before)
	}

	if err := os.WriteFile(path, []byte("replacement"), 0o600); err != nil {
		t.Fatal(err)
	}
	replacement, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer replacement.Close()
	replacementIdentity, err := IdentityFromFile(replacement)
	if err != nil {
		t.Fatal(err)
	}
	if replacementIdentity == before {
		t.Fatalf("replacement identity = %+v, want different from %+v", replacementIdentity, before)
	}
}

func TestMetadataFromFileUsesOpenHandle(t *testing.T) {
	path := filepath.Join(t.TempDir(), "rollout.jsonl")
	content := []byte("identity snapshot")
	if err := os.WriteFile(path, content, 0o600); err != nil {
		t.Fatal(err)
	}
	file, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()

	metadata, err := MetadataFromFile(file)
	if err != nil {
		t.Fatal(err)
	}
	if metadata.Size != int64(len(content)) {
		t.Fatalf("size = %d, want %d", metadata.Size, len(content))
	}
	if metadata.MTimeNS < 0 {
		t.Fatalf("mtime ns = %d, want non-negative", metadata.MTimeNS)
	}
	identity, err := IdentityFromFile(file)
	if err != nil {
		t.Fatal(err)
	}
	if metadata.Identity != identity {
		t.Fatalf("metadata identity = %+v, IdentityFromFile = %+v", metadata.Identity, identity)
	}
}

func TestFileTimeConversionsRejectOutOfRangeValues(t *testing.T) {
	if _, err := unixTimeNS(time.Unix(-1, 0)); err == nil {
		t.Fatal("unixTimeNS accepted a pre-epoch time")
	}
	if _, err := unixTimeNS(time.Unix(1<<62, 0)); err == nil {
		t.Fatal("unixTimeNS accepted an overflowing time")
	}
	if _, err := windowsTimeNS(0); err == nil {
		t.Fatal("windowsTimeNS accepted a pre-epoch time")
	}
	if _, err := windowsTimeNS(^uint64(0)); err == nil {
		t.Fatal("windowsTimeNS accepted an overflowing time")
	}
}
