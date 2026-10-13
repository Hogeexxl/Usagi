package api

import (
	"context"

	"github.com/Hogeexxl/Usagi/internal/magpie"
)

type RuntimeStatusSource interface {
	Status() magpie.RuntimeStatus
	Subscribe() (<-chan magpie.RuntimeStatus, func())
}

func startRuntimeStatusSubscription(ctx context.Context, runtime RuntimeStatusSource, revisions RevisionService) {
	statuses, cancel := runtime.Subscribe()
	var previous magpie.RuntimeStatus
	select {
	case status, ok := <-statuses:
		if !ok {
			cancel()
			return
		}
		previous = status
	case <-ctx.Done():
		cancel()
		return
	}
	go func() {
		defer cancel()
		for {
			select {
			case <-ctx.Done():
				return
			case status, ok := <-statuses:
				if !ok {
					return
				}
				if status == previous {
					continue
				}
				revisions.BumpStatus()
				previous = status
			}
		}
	}()
}
