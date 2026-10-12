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

func activationFactory(className uintptr) (uintptr, error) {
	classID := windows.GUID{Data1: 0x00000035, Data4: [8]byte{0xc0, 0, 0, 0, 0, 0, 0, 0x46}}
	var factory uintptr
	hr, _, _ := syscall.SyscallN(
		winRT.NewProc("RoGetActivationFactory").Addr(),
		className,
		uintptr(unsafe.Pointer(&classID)),
		uintptr(unsafe.Pointer(&factory)),
	)
	if err := checkHRESULT("RoGetActivationFactory", hr); err != nil {
		return 0, err
	}
	return factory, nil
}

func activate(factory uintptr) (uintptr, error) {
	var instance uintptr
	method := comMethod(factory, 6)
	hr, _, _ := syscall.SyscallN(method, factory, uintptr(unsafe.Pointer(&instance)))
	if err := checkHRESULT("IActivationFactory.ActivateInstance", hr); err != nil {
		return 0, err
	}
	return instance, nil
}

func queryTimeZoneCalendar(instance uintptr) (uintptr, error) {
	iid := windows.GUID{
		Data1: 0xbb3c25e5,
		Data2: 0x46cf,
		Data3: 0x4317,
		Data4: [8]byte{0xa3, 0xf5, 0x02, 0x62, 0x1a, 0xd5, 0x44, 0x78},
	}
	var result uintptr
	method := comMethod(instance, 0)
	hr, _, _ := syscall.SyscallN(method, instance, uintptr(unsafe.Pointer(&iid)), uintptr(unsafe.Pointer(&result)))
	if err := checkHRESULT("IUnknown.QueryInterface", hr); err != nil {
		return 0, err
	}
	return result, nil
}

func calendarTimeZone(calendar uintptr) (string, error) {
	var value uintptr
	method := comMethod(calendar, 6)
	hr, _, _ := syscall.SyscallN(method, calendar, uintptr(unsafe.Pointer(&value)))
	if err := checkHRESULT("ITimeZoneOnCalendar.GetTimeZone", hr); err != nil {
		return "", err
	}
	defer deleteHString(value)
	var length uint32
	buffer, _, _ := syscall.SyscallN(
		winRT.NewProc("WindowsGetStringRawBuffer").Addr(),
		value,
		uintptr(unsafe.Pointer(&length)),
	)
	if buffer == 0 || length == 0 {
		return "", ErrLocalTimeUnavailable
	}
	return syscall.UTF16ToString(unsafe.Slice((*uint16)(unsafe.Pointer(buffer)), int(length))), nil
}

func deleteHString(value uintptr) {
	if value != 0 {
		syscall.SyscallN(winRT.NewProc("WindowsDeleteString").Addr(), value)
	}
}

func releaseCOM(value uintptr) {
	if value != 0 {
		syscall.SyscallN(comMethod(value, 2), value)
	}
}

func comMethod(value uintptr, index int) uintptr {
	vtable := *(*uintptr)(unsafe.Pointer(value))
	return (*(*[7]uintptr)(unsafe.Pointer(vtable)))[index]
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
