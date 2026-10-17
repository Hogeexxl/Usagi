//go:build windows

package api

import (
	"fmt"
	"runtime"
	"syscall"
	"unsafe"

	"golang.org/x/sys/windows"
	_ "time/tzdata"
)

var winRT = windows.NewLazySystemDLL("combase.dll")

type winRTInterface struct {
	vtable *[7]uintptr
}

func systemTimezoneFromOS() (string, error) {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()

	uninitialize, err := initializeWinRT()
	if err != nil {
		return "", err
	}
	if uninitialize {
		defer syscall.SyscallN(winRT.NewProc("RoUninitialize").Addr())
	}

	className, err := createHString("Windows.Globalization.Calendar")
	if err != nil {
		return "", err
	}
	defer deleteHString(className)

	factory, err := activationFactory(className)
	if err != nil {
		return "", err
	}
	defer releaseCOM(factory)

	calendar, err := activate(factory)
	if err != nil {
		return "", err
	}
	defer releaseCOM(calendar)

	timeZoneCalendar, err := queryTimeZoneCalendar(calendar)
	if err != nil {
		return "", err
	}
	defer releaseCOM(timeZoneCalendar)

	name, err := calendarTimeZone(timeZoneCalendar)
	if err != nil {
		return "", err
	}
	if !validTimezoneName(name) {
		return "", ErrLocalTimeUnavailable
	}
	return name, nil
}

func initializeWinRT() (bool, error) {
	result, _, _ := syscall.SyscallN(winRT.NewProc("RoInitialize").Addr(), 1)
	if int32(result) >= 0 {
		return true, nil
	}
	if uint32(result) == 0x80010106 { // RPC_E_CHANGED_MODE: the current apartment is usable.
		return false, nil
	}
	return false, hresultError("RoInitialize", result)
}

func createHString(value string) (uintptr, error) {
	wide, err := syscall.UTF16FromString(value)
	if err != nil {
		return 0, err
	}
	var result uintptr
	hr, _, _ := syscall.SyscallN(
		winRT.NewProc("WindowsCreateString").Addr(),
		uintptr(unsafe.Pointer(&wide[0])),
		uintptr(len(wide)-1),
		uintptr(unsafe.Pointer(&result)),
	)
	if err := checkHRESULT("WindowsCreateString", hr); err != nil {
		return 0, err
	}
	return result, nil
}

func activationFactory(className uintptr) (*winRTInterface, error) {
	classID := windows.GUID{Data1: 0x00000035, Data4: [8]byte{0xc0, 0, 0, 0, 0, 0, 0, 0x46}}
	var factory *winRTInterface
	hr, _, _ := syscall.SyscallN(
		winRT.NewProc("RoGetActivationFactory").Addr(),
		className,
		uintptr(unsafe.Pointer(&classID)),
		uintptr(unsafe.Pointer(&factory)),
	)
	if err := checkHRESULT("RoGetActivationFactory", hr); err != nil {
		return nil, err
	}
	return factory, nil
}

func activate(factory *winRTInterface) (*winRTInterface, error) {
	var instance *winRTInterface
	method := comMethod(factory, 6)
	hr, _, _ := syscall.SyscallN(method, uintptr(unsafe.Pointer(factory)), uintptr(unsafe.Pointer(&instance)))
	if err := checkHRESULT("IActivationFactory.ActivateInstance", hr); err != nil {
		return nil, err
	}
	return instance, nil
}

func queryTimeZoneCalendar(instance *winRTInterface) (*winRTInterface, error) {
	iid := windows.GUID{
		Data1: 0xbb3c25e5,
		Data2: 0x46cf,
		Data3: 0x4317,
		Data4: [8]byte{0xa3, 0xf5, 0x02, 0x62, 0x1a, 0xd5, 0x44, 0x78},
	}
	var result *winRTInterface
	method := comMethod(instance, 0)
	hr, _, _ := syscall.SyscallN(method, uintptr(unsafe.Pointer(instance)), uintptr(unsafe.Pointer(&iid)), uintptr(unsafe.Pointer(&result)))
	if err := checkHRESULT("IUnknown.QueryInterface", hr); err != nil {
		return nil, err
	}
	return result, nil
}

func calendarTimeZone(calendar *winRTInterface) (string, error) {
	var value uintptr
	method := comMethod(calendar, 6)
	hr, _, _ := syscall.SyscallN(method, uintptr(unsafe.Pointer(calendar)), uintptr(unsafe.Pointer(&value)))
	if err := checkHRESULT("ITimeZoneOnCalendar.GetTimeZone", hr); err != nil {
		return "", err
	}
	defer deleteHString(value)
	var length uint32
	bufferAddress, _, _ := syscall.SyscallN(
		winRT.NewProc("WindowsGetStringRawBuffer").Addr(),
		value,
		uintptr(unsafe.Pointer(&length)),
	)
	// SyscallN exposes native pointer returns as uintptr. Recover the pointer
	// at this boundary; the deferred HSTRING deletion keeps its buffer alive.
	buffer := *(**uint16)(unsafe.Pointer(&bufferAddress))
	if buffer == nil || length == 0 {
		return "", ErrLocalTimeUnavailable
	}
	return syscall.UTF16ToString(unsafe.Slice(buffer, int(length))), nil
}

func deleteHString(value uintptr) {
	if value != 0 {
		syscall.SyscallN(winRT.NewProc("WindowsDeleteString").Addr(), value)
	}
}

func releaseCOM(value *winRTInterface) {
	if value != nil {
		syscall.SyscallN(comMethod(value, 2), uintptr(unsafe.Pointer(value)))
	}
}

func comMethod(value *winRTInterface, index int) uintptr {
	return value.vtable[index]
}

func checkHRESULT(operation string, result uintptr) error {
	if int32(result) < 0 {
		return hresultError(operation, result)
	}
	return nil
}

func hresultError(operation string, result uintptr) error {
	return fmt.Errorf("api: %s failed with HRESULT 0x%08x", operation, uint32(result))
}
