//go:build !windows

package app

import "syscall"

const addressInUseError = syscall.EADDRINUSE
