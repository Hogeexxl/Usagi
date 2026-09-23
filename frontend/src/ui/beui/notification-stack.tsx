"use client";

import { motion, useReducedMotion } from "motion/react";
import {
  useState,
  type KeyboardEvent,
  type MouseEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { EASE_OUT } from "../lib/ease";
import { useDismiss } from "../lib/use-dismiss";
import { useTapGesture } from "../lib/use-tap-gesture";

export type NotificationStackItem = {
  id: string;
  content: ReactNode;
};

type NotificationStackProps = {
  items: NotificationStackItem[];
  expanded: boolean;
  dismissRef: RefObject<HTMLElement | null>;
  onExpandedChange: (expanded: boolean) => void;
  className?: string;
};

const STACK_PEEK = 8;
const STACK_INSET = 12;
const QUOTA_CARD_HEIGHT = 92;
const QUOTA_CARD_GAP = 4;
const QUOTA_CARD_STEP = QUOTA_CARD_HEIGHT + QUOTA_CARD_GAP;

export function NotificationStack({
  items,
  expanded,
  dismissRef,
  onExpandedChange,
  className = "relative h-[100px] w-[216px] outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
}: NotificationStackProps) {
  const reduce = useReducedMotion();
  const tap = useTapGesture<boolean>();
  const [tapExpanded, setTapExpanded] = useState(false);
  const cardTransition = reduce ? { duration: 0 } : { duration: 0.32, ease: EASE_OUT };
  const expandedHeight = Math.max(100, items.length * QUOTA_CARD_HEIGHT + (items.length - 1) * QUOTA_CARD_GAP);

  useDismiss(
    tapExpanded && expanded,
    () => {
      setTapExpanded(false);
      onExpandedChange(false);
    },
    dismissRef,
    { behavior: "consume" },
  );

  const collapse = () => {
    setTapExpanded(false);
    onExpandedChange(false);
  };

  const handleKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      collapse();
      return;
    }
    if (event.target !== event.currentTarget) return;
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      onExpandedChange(!expanded);
      setTapExpanded(false);
    }
  };

  const handleClick = (event: MouseEvent<HTMLDivElement>) => {
    const target = event.target as Element;
    if (target.closest("button, a, [role='button'], [data-stack-interactive]")) {
      tap.drop();
      return;
    }
    const gesture = tap.take();
    const wasExpanded = gesture ? gesture.state : expanded;
    if (wasExpanded) {
      collapse();
      return;
    }
    onExpandedChange(true);
    if (gesture && gesture.pointerType !== "mouse") setTapExpanded(true);
  };

  return (
    <motion.div
      role="group"
      tabIndex={0}
      aria-label="账户额度卡片"
      aria-expanded={expanded}
      onFocus={() => onExpandedChange(true)}
      onPointerDown={(event) => tap.start(event, expanded)}
      onPointerCancel={tap.drop}
      onKeyDown={handleKeyDown}
      onClick={handleClick}
      className={className}
      style={{ zIndex: expanded ? 1 : 0, height: expanded ? expandedHeight : undefined }}
    >
      {items.map((item, index) => (
        <motion.div
          key={item.id}
          layout="position"
          initial={false}
          animate={{
            y: expanded ? index * QUOTA_CARD_STEP : index * STACK_PEEK,
            clipPath: expanded
              ? "inset(0px 0px round 12px)"
              : `inset(0px ${index * STACK_INSET}px round 12px)`,
          }}
          transition={cardTransition}
          className="absolute left-0 top-0 h-[92px] w-full rounded-xl border border-border/60 bg-background p-[10px] text-foreground shadow-sm"
          style={{
            zIndex: items.length - index,
            pointerEvents: expanded || index === 0 ? "auto" : "none",
            visibility: expanded || index < 2 ? "visible" : "hidden",
          }}
          aria-hidden={!expanded && index > 0}
          inert={!expanded && index > 0}
        >
          {item.content}
        </motion.div>
      ))}
    </motion.div>
  );
}
