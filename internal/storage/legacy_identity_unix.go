//go:build !windows

package storage

import (
	"os"
	"syscall"
)

func validateConversionVolume(string) error { return nil }

func readSourceIdentity(path string) (SourceFileIdentity, error) {
	info, err := os.Stat(path)
	if err != nil {
		return SourceFileIdentity{}, newStorageError(ErrorIO, err)
	}
	stat := info.Sys().(*syscall.Stat_t)
	return SourceFileIdentity{Platform: "unix", Device: uint64(stat.Dev), Inode: uint64(stat.Ino)}, nil
}

func syncDirectory(path string) error {
	directory, err := os.Open(path)
	if err != nil {
		return newStorageError(ErrorIO, err)
	}
	defer directory.Close()
	return newStorageError(ErrorIO, directory.Sync())
}
