//go:build production

package webui

import (
	"embed"
	"io/fs"
)

//go:embed dist
var embedded embed.FS

func FS() fs.FS {
	root, err := fs.Sub(embedded, "dist")
	if err != nil {
		panic(err)
	}
	return root
}
