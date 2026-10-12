//go:build !production

package webui

import (
	"io/fs"
	"os"
)

func FS() fs.FS {
	return os.DirFS("frontend/dist")
}
