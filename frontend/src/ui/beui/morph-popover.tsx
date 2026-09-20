"use client";

import { motion, type Transition, useReducedMotion } from "motion/react";
import {
  cloneElement,
  createContext,
  isValidElement,
  type CSSProperties,
  type ReactElement,
  type ReactNode,
  type Ref,
  useCallback,
  useContext,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import { usePopoverPortalPosition } from "./popover-position";
import { cn } from "../lib/cn";

type Side = "top" | "bottom";
type Align = "start" | "center" | "end";

type MorphContextValue = {
  open: boolean;
  setOpen: (open: boolean) => void;
  toggle: () => void;
  triggerId: string;
  contentId: string;
  /** The element the panel measures against — see `registerTrigger`. */
  triggerRef: React.MutableRefObject<HTMLElement | null>;
  registerTrigger: (node: HTMLElement | null) => void;
  contentRef: React.MutableRefObject<HTMLDivElement | null>;
};

const MorphContext = createContext<MorphContextValue | null>(null);

function useMorphContext(component: string) {
  const ctx = useContext(MorphContext);
  if (!ctx) throw new Error(`${component} must be used within <MorphPopover>`);
  return ctx;
}

export interface MorphPopoverProps {
  children: ReactNode;
  /** Controlled open state. */
  open?: boolean;
  /** Uncontrolled initial open state. */
  defaultOpen?: boolean;
  onOpenChange?: (open: boolean) => void;
  className?: string;
}

/**
 * A popover whose panel morphs open from the trigger corner: it's laid out at
 * full size but clipped to the corner nearest the trigger, then unclips as one
 * piece. Closes on outside pointer / Escape. Controlled or uncontrolled.
 */
export function MorphPopover({
  children,
  open: controlledOpen,
  defaultOpen = false,
  onOpenChange,
  className,
}: MorphPopoverProps) {
  const baseId = useId();
  const [root, setRoot] = useState<HTMLDivElement | null>(null);
  const [trigger, setTrigger] = useState<HTMLElement | null>(null);
  const contentRef = useRef<HTMLDivElement | null>(null);
  const [internalOpen, setInternalOpen] = useState(defaultOpen);
  const controlled = controlledOpen !== undefined;
  const open = controlled ? controlledOpen : internalOpen;

  const setOpen = useCallback(
    (next: boolean) => {
      if (!controlled) setInternalOpen(next);
      onOpenChange?.(next);
    },
    [controlled, onOpenChange],
  );
  const toggle = useCallback(() => setOpen(!open), [setOpen, open]);

  // A trigger normally registers itself through MorphPopoverTrigger. It can't
  // when something else already clones the element — a Tooltip wrapping the
  // button, say — and an unregistered trigger leaves the panel with nothing to
  // measure against, so it renders permanently invisible. The root boxes the
  // trigger exactly (the content portals out of it), so it stands in until a
  // real trigger registers, and stands in again if that one unmounts. Both are
  // state, so a trigger arriving while the panel is open re-anchors it.
  const anchorRef = useMemo<React.MutableRefObject<HTMLElement | null>>(
    () => ({ current: trigger ?? root }),
    [root, trigger],
  );

  // The panel is a `role="dialog"` and goes inert the moment it closes, so
  // focus cannot be left sitting inside it: a dismissal hands it back to the
  // trigger, the way the ARIA dialog pattern asks. A pointer dismissal takes
  // the focus onward itself when it lands on something focusable — this only
  // catches the case where it would otherwise be stranded. When no trigger has
  // registered, the root anchor stands in only if it can actually hold focus;
  // there is nowhere better than where the keyboard already is, so leave it.
  const close = useCallback(() => {
    setOpen(false);
    const focused = document.activeElement;
    const inPanel =
      focused instanceof HTMLElement && contentRef.current?.contains(focused);
    if (!inPanel) return;
    const restore = trigger ?? (root && root.tabIndex >= 0 ? root : null);
    restore?.focus();
  }, [root, setOpen, trigger]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && close();
    const onPointer = (e: PointerEvent) => {
      const target = e.target as Node;
      if (
        root &&
        !root.contains(target) &&
        !contentRef.current?.contains(target)
      )
        close();
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("pointerdown", onPointer);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("pointerdown", onPointer);
    };
  }, [open, root, close]);

  const ctx = useMemo<MorphContextValue>(
    () => ({
      open,
      setOpen,
      toggle,
      triggerId: `${baseId}-trigger`,
      contentId: `${baseId}-content`,
      triggerRef: anchorRef,
      registerTrigger: setTrigger,
      contentRef,
    }),
    [open, setOpen, toggle, baseId, anchorRef],
  );

  return (
    <MorphContext.Provider value={ctx}>
      <div ref={setRoot} className={cn("relative inline-flex", className)}>
        {children}
      </div>
    </MorphContext.Provider>
  );
}

export interface MorphPopoverTriggerProps {
  children: ReactElement;
}

function mergeRefs<T>(...refs: Array<Ref<T> | undefined>) {
  return (node: T | null) => {
    for (const ref of refs) {
      if (typeof ref === "function") ref(node);
      else if (ref && typeof ref === "object")
        (ref as React.MutableRefObject<T | null>).current = node;
    }
  };
}

/** Wraps a single element, toggling the popover on click. */
export function MorphPopoverTrigger({ children }: MorphPopoverTriggerProps) {
  const ctx = useMorphContext("MorphPopoverTrigger");
  if (!isValidElement(children)) return children;

  const child = children as ReactElement<Record<string, unknown>>;
  const childOnClick = child.props.onClick as
    | ((e: unknown) => void)
    | undefined;
  const childRef = (child.props as { ref?: Ref<HTMLElement> }).ref;

  return cloneElement(child, {
    id: ctx.triggerId,
    ref: mergeRefs(childRef, ctx.registerTrigger),
    onClick: (e: unknown) => {
      childOnClick?.(e);
      ctx.toggle();
    },
    "aria-haspopup": "dialog",
    "aria-expanded": ctx.open,
    "aria-controls": ctx.open ? ctx.contentId : undefined,
  });
}

// Panel/motion architecture copied from beUI Motion Multi Select content.tsx.
// Keep this motion contract in sync with https://beui.dev/components/motion/multi-select.
const MULTI_SELECT_MORPH: Transition = {
  type: "spring",
  duration: 0.5,
  bounce: 0.22,
};
const VIEWPORT_PADDING = 8;

export interface MorphPopoverContentProps {
  children: ReactNode;
  side?: Side;
  align?: Align;
  sideOffset?: number;
  avoidCollisions?: boolean;
  /** Panel corner radius in px. */
  radius?: number;
  className?: string;
}

export function MorphPopoverContent({
  children,
  side = "bottom",
  align = "start",
  sideOffset = 6,
  avoidCollisions = true,
  radius = 16,
  className,
}: MorphPopoverContentProps) {
  const ctx = useMorphContext("MorphPopoverContent");
  const reduce = useReducedMotion() ?? false;
  const measureRef = useRef<HTMLDivElement>(null);
  const [portalReady, setPortalReady] = useState(false);
  const [actualSide, setActualSide] = useState<Side>(side);
  const [morphReady, setMorphReady] = useState(false);
  const layout = usePopoverPortalPosition(
    ctx.triggerRef,
    measureRef,
    portalReady,
  );

  useEffect(() => setPortalReady(true), []);
  useLayoutEffect(() => {
    if (!portalReady) return;
    const readyFrame = requestAnimationFrame(() => setMorphReady(true));
    return () => cancelAnimationFrame(readyFrame);
  }, [portalReady]);

  useLayoutEffect(() => {
    if (!ctx.open || !layout) return;
    if (!avoidCollisions) {
      setActualSide(side);
      return;
    }
    const below =
      window.innerHeight - (layout.trigger.top + layout.trigger.height);
    const above = layout.trigger.top;
    if (
      side === "bottom" &&
      below < layout.content.height + sideOffset &&
      above > below
    ) {
      setActualSide("top");
    } else if (
      side === "top" &&
      above < layout.content.height + sideOffset &&
      below > above
    ) {
      setActualSide("bottom");
    } else {
      setActualSide(side);
    }
  }, [avoidCollisions, ctx.open, layout, side, sideOffset]);

  if (!portalReady) return null;

  const triggerLeft = layout?.trigger.left ?? 0;
  const triggerWidth = layout?.trigger.width ?? 0;
  const contentWidth = layout?.content.width ?? triggerWidth;
  const desiredLeft =
    align === "end"
      ? triggerLeft + triggerWidth - contentWidth
      : align === "center"
        ? triggerLeft + (triggerWidth - contentWidth) / 2
        : triggerLeft;
  const maxLeft = Math.max(
    VIEWPORT_PADDING,
    window.innerWidth - contentWidth - VIEWPORT_PADDING,
  );
  const left = Math.min(Math.max(desiredLeft, VIEWPORT_PADDING), maxLeft);
  const surfaceHeight = layout?.content.height ?? 0;

  return createPortal(
    <motion.div
      ref={ctx.contentRef}
      id={ctx.contentId}
      role="dialog"
      aria-labelledby={ctx.triggerId}
      data-multi-select-content=""
      data-side={actualSide}
      aria-hidden={!ctx.open}
      inert={!ctx.open}
      initial={false}
      animate={{
        height: ctx.open ? surfaceHeight : 0,
        opacity: ctx.open ? 1 : 0,
        y: ctx.open
          ? actualSide === "bottom"
            ? sideOffset
            : -sideOffset
          : 0,
      }}
      transition={
        reduce || !morphReady ? { duration: 0 } : MULTI_SELECT_MORPH
      }
      style={
        {
          left,
          top:
            actualSide === "bottom" && layout
              ? layout.trigger.top + layout.trigger.height
              : undefined,
          bottom:
            actualSide === "top" && layout
              ? window.innerHeight - layout.trigger.top
              : undefined,
          minWidth: triggerWidth,
          pointerEvents: ctx.open ? "auto" : "none",
          transformOrigin: actualSide === "bottom" ? "top" : "bottom",
          visibility: layout ? "visible" : "hidden",
          borderRadius: radius,
          "--multi-select-trigger-width": `${triggerWidth}px`,
        } as CSSProperties
      }
      className={cn(
        "fixed z-[9999] w-(--multi-select-trigger-width) overflow-hidden border border-border bg-background text-popover-foreground outline-none will-change-[height,transform] [filter:drop-shadow(0_10px_18px_rgba(0,0,0,0.14))]",
        className,
      )}
    >
      <motion.div
        ref={measureRef}
        initial={false}
        animate={{ opacity: ctx.open ? 1 : 0 }}
        transition={
          reduce || !morphReady ? { duration: 0 } : MULTI_SELECT_MORPH
        }
      >
        {children}
      </motion.div>
    </motion.div>,
    document.body,
  );
}
