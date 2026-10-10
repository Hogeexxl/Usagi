//go:build windows

package platform

import (
	"encoding/binary"
	"fmt"
	"math"
	"os"

	"github.com/zeebo/blake3"
	"golang.org/x/sys/windows"
)

func IdentityFromFile(f *os.File) (FileIdentity, error) {
	var info windows.ByHandleFileInformation
	if err := windows.GetFileInformationByHandle(windows.Handle(f.Fd()), &info); err != nil {
		return FileIdentity{}, fmt.Errorf("read open file information: %w", err)
	}
	return identityFromWindowsInfo(info), nil
}

func MetadataFromFile(f *os.File) (FileMetadata, error) {
	var info windows.ByHandleFileInformation
	if err := windows.GetFileInformationByHandle(windows.Handle(f.Fd()), &info); err != nil {
		return FileMetadata{}, fmt.Errorf("read open file information: %w", err)
	}
	fileSize := uint64(info.FileSizeHigh)<<32 | uint64(info.FileSizeLow)
	if fileSize > uint64(math.MaxInt64) {
		return FileMetadata{}, fmt.Errorf("file size exceeds SQLite integer range")
	}
	identity := identityFromWindowsInfo(info)
	fileTimeTicks := uint64(info.LastWriteTime.HighDateTime)<<32 | uint64(info.LastWriteTime.LowDateTime)
	mtimeNS, err := windowsTimeNS(fileTimeTicks)
	if err != nil {
		return FileMetadata{}, err
	}
	return FileMetadata{Identity: identity, Size: int64(fileSize), MTimeNS: mtimeNS}, nil
}

func identityFromWindowsInfo(info windows.ByHandleFileInformation) FileIdentity {
	fileIndex := uint64(info.FileIndexHigh)<<32 | uint64(info.FileIndexLow)
	raw := make([]byte, 12)
	binary.LittleEndian.PutUint32(raw[:4], info.VolumeSerialNumber)
	binary.LittleEndian.PutUint64(raw[4:], fileIndex)
	sum := blake3.Sum256(raw)
	device := binary.LittleEndian.Uint64(sum[:8]) & uint64(math.MaxInt64)
	inode := binary.LittleEndian.Uint64(sum[8:16]) & uint64(math.MaxInt64)
	if device == 0 && inode == 0 {
		inode = 1
	}
	return FileIdentity{DeviceID: int64(device), Inode: int64(inode)}
}
