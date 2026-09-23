import { useEffect, useRef, useState } from "react";
import { LoaderCircle, Power } from "lucide-react";

import { serviceClient, type ServiceClient, type ServiceState } from "../data/serviceClient";
import { AnimatedToastStack, useAnimatedToastStack } from "../ui/beui/animated-toast-stack";
import { Button } from "../ui/beui/button";

type ViewState = ServiceState | "loading" | "stopping" | "error";

function isAbortError(error: unknown): boolean {
  return error instanceof DOMException && error.name === "AbortError";
}

export function ServiceButton({ client = serviceClient }: { client?: ServiceClient }) {
  const [state, setState] = useState<ViewState>("loading");
  const requestRef = useRef<AbortController | null>(null);
  const toast = useAnimatedToastStack();

  useEffect(() => {
    const controller = new AbortController();
    requestRef.current = controller;
    void client.getState(controller.signal).then(
      (next) => setState(next),
      (error: unknown) => {
        if (isAbortError(error)) return;
        setState("error");
        toast.showToast({ status: "error", title: "服务状态读取失败" });
      },
    );
    return () => requestRef.current?.abort();
  }, [client, toast.showToast]);

  const stopService = () => {
    if (state !== "running") return;
    requestRef.current?.abort();
    const controller = new AbortController();
    requestRef.current = controller;
    setState("stopping");
    const toastId = toast.showToast({
      status: "loading",
      title: "正在停止服务",
      duration: 0,
      dismissible: false,
    });
    void client.stop(controller.signal).then(
      (next) => {
        setState(next);
        toast.updateToast(toastId, {
          status: "success",
          title: "服务已停止",
          duration: undefined,
          dismissible: true,
        });
      },
      (error: unknown) => {
        if (isAbortError(error)) return;
        setState("running");
        toast.updateToast(toastId, {
          status: "error",
          title: "停止服务失败",
          duration: undefined,
          dismissible: true,
        });
      },
    );
  };

  return (
    <>
      <Button
        variant="ghost"
        size="icon"
        aria-label="停止服务"
        title={state === "stopping" ? "停止中…" : "停止服务"}
        disabled={state !== "running"}
        onClick={stopService}
        className="text-destructive hover:bg-destructive/10 hover:text-destructive"
      >
        {state === "stopping" ? <LoaderCircle className="h-4 w-4 animate-spin" /> : <Power className="h-4 w-4" />}
      </Button>
      <AnimatedToastStack
        toasts={toast.toasts}
        onDismiss={toast.dismissToast}
        placement="fixed"
      />
    </>
  );
}
