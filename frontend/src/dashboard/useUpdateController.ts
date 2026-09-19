import { useCallback, useEffect, useRef, useState } from "react";

import {
  usagiClient,
  type UsagiClient,
  type UsagiUpdateClient,
} from "../data/usagiClient";
import { type UpdateStatusResponse } from "../data/types";

const DEFAULT_POLL_INTERVAL_MS = 60_000;

export type UpdateControllerOptions = {
  client?: UsagiClient;
  pollIntervalMs?: number;
};

export type UpdateViewModel = {
  status: UpdateStatusResponse | null;
  open_release: () => void;
};

function hasUpdateStatusApi(client: UsagiClient): client is UsagiClient & Pick<UsagiUpdateClient, "getUpdateStatus"> {
  return typeof client.getUpdateStatus === "function";
}

function hasOpenReleaseApi(client: UsagiClient): client is UsagiClient & Pick<UsagiUpdateClient, "openRelease"> {
  return typeof client.openRelease === "function";
}

export function useUpdateController(options: UpdateControllerOptions = {}): UpdateViewModel {
  const client = options.client ?? usagiClient;
  const pollIntervalMs = options.pollIntervalMs ?? DEFAULT_POLL_INTERVAL_MS;
  const supportsStatus = hasUpdateStatusApi(client);
  const supportsOpenRelease = hasOpenReleaseApi(client);
  const [status, setStatus] = useState<UpdateStatusResponse | null>(null);
  const statusRef = useRef<UpdateStatusResponse | null>(null);
  const mountedRef = useRef(false);
  const statusRequestRef = useRef<AbortController | null>(null);
  const statusGenerationRef = useRef(0);
  const openRequestRef = useRef<AbortController | null>(null);

  const requestStatus = useCallback(() => {
    if (!supportsStatus || statusRequestRef.current) return;
    const controller = new AbortController();
    const generation = ++statusGenerationRef.current;
    statusRequestRef.current = controller;
    void client.getUpdateStatus(controller.signal).then(
      (response) => {
        if (!mountedRef.current || controller.signal.aborted || generation !== statusGenerationRef.current) return;
        statusRef.current = response;
        setStatus(response);
      },
      () => {
        // Status polling is deliberately silent. The current known state remains visible.
      },
    ).finally(() => {
      if (statusRequestRef.current === controller) statusRequestRef.current = null;
    });
  }, [client, supportsStatus]);

  const openRelease = useCallback(() => {
    if (!supportsOpenRelease || !statusRef.current?.update_available || openRequestRef.current) return;
    const controller = new AbortController();
    openRequestRef.current = controller;
    void client.openRelease(controller.signal).catch(() => undefined).finally(() => {
      if (openRequestRef.current === controller) openRequestRef.current = null;
    });
  }, [client, supportsOpenRelease]);

  useEffect(() => {
    mountedRef.current = true;
    if (!supportsStatus) {
      return () => {
        mountedRef.current = false;
        statusRequestRef.current?.abort();
        statusRequestRef.current = null;
        openRequestRef.current?.abort();
        openRequestRef.current = null;
      };
    }

    requestStatus();
    const timer = window.setInterval(requestStatus, pollIntervalMs);
    return () => {
      mountedRef.current = false;
      window.clearInterval(timer);
      statusRequestRef.current?.abort();
      statusRequestRef.current = null;
      openRequestRef.current?.abort();
      openRequestRef.current = null;
    };
  }, [pollIntervalMs, requestStatus, supportsStatus]);

  return {
    status,
    open_release: openRelease,
  };
}
