package main

import (
	"context"
	"log"
	"os"
	"os/signal"
	"syscall"

	"github.com/Hogeexxl/Usagi/internal/app"
	"github.com/Hogeexxl/Usagi/internal/magpie"
	"github.com/Hogeexxl/Usagi/internal/platform"
	"github.com/Hogeexxl/Usagi/internal/webui"
)

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	factory := func() app.MagpieRuntime {
		return magpie.NewRuntime()
	}
	if err := app.Run(ctx, app.Dependencies{
		RuntimeFactory: factory,
		Browser:        platform.Browser{},
		StaticFS:       webui.FS(),
	}); err != nil {
		log.Fatal(err)
	}
}
