package main

import (
	"context"
	"flag"
	"fmt"
	"os"

	"github.com/Hogeexxl/Usagi/internal/storage"
)

func main() {
	os.Exit(run())
}

func run() (exitCode int) {
	dbPath := flag.String("db", "", "database path (required)")
	legacySource := flag.String("legacy-source", "", "legacy database source path")
	flag.Parse()
	if *dbPath == "" {
		fmt.Fprintln(os.Stderr, "--db is required")
		return 2
	}

	ctx := context.Background()
	if *legacySource != "" {
		if err := storage.ConvertLegacy(ctx, storage.LegacyConversionConfig{
			SourcePath: *legacySource,
			TargetPath: *dbPath,
		}); err != nil {
			fmt.Fprintln(os.Stderr, err)
			return 1
		}
	}
	db, err := storage.Open(ctx, storage.Config{Path: *dbPath})
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	defer func() {
		if err := db.Close(); err != nil {
			fmt.Fprintln(os.Stderr, err)
			exitCode = 1
		}
	}()

	version, err := db.SchemaVersion(ctx)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	if err := db.Validate(ctx); err != nil {
		fmt.Fprintln(os.Stderr, err)
		return 1
	}
	fmt.Printf("schema_generation=%d\nschema_version=%d\nvalidation=ok\n", version.Generation, version.Version)
	return 0
}
