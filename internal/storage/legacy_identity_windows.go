//go:build windows

package storage

import (
	"encoding/hex"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"unsafe"

	"golang.org/x/sys/windows"
)

func validateConversionVolume(target string) error {
	// Resolve the nearest existing parent through a handle, including junctions
	// and mounted folders. A volume GUID path identifies a local volume.
	parent := filepath.Dir(target)
	for {
		_, err := os.Stat(parent)
		if err == nil {
			break
		}
		if !errors.Is(err, os.ErrNotExist) || filepath.Dir(parent) == parent {
			return newStorageError(ErrorInvalidState, err)
		}
		parent = filepath.Dir(parent)
	}
	name, err := windows.UTF16PtrFromString(parent)
	if err != nil {
		return newStorageError(ErrorInvalidState, err)
	}
	handle, err := windows.CreateFile(name, windows.GENERIC_READ, windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE|windows.FILE_SHARE_DELETE, nil, windows.OPEN_EXISTING, windows.FILE_FLAG_BACKUP_SEMANTICS, 0)
	if err != nil {
		return newStorageError(ErrorInvalidState, err)
	}
	defer windows.CloseHandle(handle)
	resolved := make([]uint16, 32768)
	const volumeNameGUID = 0x1 // Win32 VOLUME_NAME_GUID.
	n, err := windows.GetFinalPathNameByHandle(handle, &resolved[0], uint32(len(resolved)), volumeNameGUID)
	if err != nil {
		return newStorageError(ErrorInvalidState, err)
	}
	if n == 0 || n >= uint32(len(resolved)) {
		return newStorageError(ErrorInvalidState, errors.New("cannot resolve conversion target volume"))
	}
	path := windows.UTF16ToString(resolved[:n])
	end := strings.Index(path, `}\`)
	if !strings.HasPrefix(path, `\\?\Volume{`) || end < 0 {
		return newStorageError(ErrorInvalidState, errors.New("conversion requires a local NTFS target volume"))
	}
	root, err := windows.UTF16PtrFromString(path[:end+2])
	if err != nil {
		return newStorageError(ErrorInvalidState, err)
	}
	switch windows.GetDriveType(root) {
	case windows.DRIVE_REMOVABLE, windows.DRIVE_FIXED, windows.DRIVE_CDROM, windows.DRIVE_RAMDISK:
	default:
		return newStorageError(ErrorInvalidState, errors.New("conversion requires a local target volume"))
	}
	var filesystem [256]uint16
	if err := windows.GetVolumeInformationByHandle(handle, nil, 0, nil, nil, nil, &filesystem[0], uint32(len(filesystem))); err != nil {
		return newStorageError(ErrorInvalidState, err)
	}
	if !strings.EqualFold(windows.UTF16ToString(filesystem[:]), "NTFS") {
		return newStorageError(ErrorInvalidState, errors.New("conversion requires an NTFS target volume"))
	}
	return nil
}

func readSourceIdentity(path string) (SourceFileIdentity, error) {
	name, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return SourceFileIdentity{}, newStorageError(ErrorIO, err)
	}
	handle, err := windows.CreateFile(name, windows.GENERIC_READ, windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE|windows.FILE_SHARE_DELETE, nil, windows.OPEN_EXISTING, 0, 0)
	if err != nil {
		return SourceFileIdentity{}, newStorageError(ErrorIO, err)
	}
	defer windows.CloseHandle(handle)
	// FILE_ID_INFO has a 64-bit volume serial followed by FILE_ID_128.
	var identity struct {
		VolumeSerialNumber uint64
		FileID             [16]byte
	}
	if err := windows.GetFileInformationByHandleEx(handle, windows.FileIdInfo, (*byte)(unsafe.Pointer(&identity)), uint32(unsafe.Sizeof(identity))); err != nil {
		return SourceFileIdentity{}, newStorageError(ErrorIO, err)
	}
	return SourceFileIdentity{Platform: "windows", VolumeSerialNumber: identity.VolumeSerialNumber, FileID128: hex.EncodeToString(identity.FileID[:])}, nil
}

func syncDirectory(path string) error {
	name, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return newStorageError(ErrorIO, err)
	}
	handle, err := windows.CreateFile(name, windows.GENERIC_READ|windows.GENERIC_WRITE, windows.FILE_SHARE_READ|windows.FILE_SHARE_WRITE|windows.FILE_SHARE_DELETE, nil, windows.OPEN_EXISTING, windows.FILE_FLAG_BACKUP_SEMANTICS, 0)
	if err != nil {
		return newStorageError(ErrorIO, err)
	}
	defer windows.CloseHandle(handle)
	return newStorageError(ErrorIO, windows.FlushFileBuffers(handle))
}
