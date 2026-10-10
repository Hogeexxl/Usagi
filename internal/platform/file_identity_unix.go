//go:build !windows

package platform

import (
	"fmt"
	"math"
	"os"
	"syscall"
)

func IdentityFromFile(f *os.File) (FileIdentity, error) {
	info, err := f.Stat()
	if err != nil {
		return FileIdentity{}, fmt.Errorf("stat open file: %w", err)
	}
	return identityFromFileInfo(info)
}

func MetadataFromFile(f *os.File) (FileMetadata, error) {
	info, err := f.Stat()
	if err != nil {
		return FileMetadata{}, fmt.Errorf("stat open file: %w", err)
	}
	identity, err := identityFromFileInfo(info)
	if err != nil {
		return FileMetadata{}, err
	}
	mtimeNS, err := unixTimeNS(info.ModTime())
	if err != nil {
		return FileMetadata{}, err
	}
	if info.Size() < 0 {
		return FileMetadata{}, fmt.Errorf("file size is negative")
	}
	return FileMetadata{Identity: identity, Size: info.Size(), MTimeNS: mtimeNS}, nil
}

func identityFromFileInfo(info os.FileInfo) (FileIdentity, error) {
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok {
		return FileIdentity{}, fmt.Errorf("open file metadata has unexpected type %T", info.Sys())
	}
	device, inode := uint64(stat.Dev), uint64(stat.Ino)
	if device > uint64(math.MaxInt64) || inode > uint64(math.MaxInt64) {
		return FileIdentity{}, fmt.Errorf("file identity exceeds SQLite integer range")
	}
	return FileIdentity{DeviceID: int64(device), Inode: int64(inode)}, nil
}
