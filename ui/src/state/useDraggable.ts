//! Window-style dragging for overlay surfaces (doc 11).
//
//  Threshold semantics: a press only becomes a drag after ~6 px of movement,
//  so buttons inside the handle area keep working — once the drag begins we
//  take pointer capture, which retargets the eventual click away from the
//  pressed control (a drag never triggers the button it started on).
//
//  While dragging, the overlay holds the modal interactive override: the
//  hit-test rect publisher (250 ms cadence) is too slow to follow a moving
//  surface, and losing interactivity mid-drag would drop the gesture.

import { useRef, useState } from "react";
import type { CSSProperties, PointerEvent as ReactPointerEvent, RefObject } from "react";

import { setOverlayInteractive } from "../lib/ipc";

const DRAG_THRESHOLD_PX = 6;

export interface Draggable {
  /** Spread onto the surface root — positions it once the user has dragged. */
  style: CSSProperties | undefined;
  dragging: boolean;
  /** Spread onto the drag handle (a titlebar, a header, or the whole surface). */
  handleProps: {
    onPointerDown: (e: ReactPointerEvent<HTMLElement>) => void;
    onPointerMove: (e: ReactPointerEvent<HTMLElement>) => void;
    onPointerUp: (e: ReactPointerEvent<HTMLElement>) => void;
    onPointerCancel: (e: ReactPointerEvent<HTMLElement>) => void;
  };
}

export function useDraggable(
  ref: RefObject<HTMLElement | null>,
  opts?: {
    /** Called with the drop point (pointer coords) when a drag ends. */
    onDrop?: (x: number, y: number) => void;
    /** When true, the position resets to CSS defaults after `onDrop` (for
     *  surfaces that snap to anchors instead of staying where dropped). */
    resetOnDrop?: boolean;
  },
): Draggable {
  const [pos, setPos] = useState<{ x: number; y: number } | null>(null);
  const [dragging, setDragging] = useState(false);
  const gesture = useRef<{
    startX: number;
    startY: number;
    dx: number;
    dy: number;
    active: boolean;
  } | null>(null);

  function onPointerDown(e: ReactPointerEvent<HTMLElement>) {
    if (e.button !== 0 || !ref.current) return;
    const rect = ref.current.getBoundingClientRect();
    gesture.current = {
      startX: e.clientX,
      startY: e.clientY,
      dx: e.clientX - rect.left,
      dy: e.clientY - rect.top,
      active: false,
    };
    // No capture and no preventDefault yet — a plain click must stay a click.
  }

  function onPointerMove(e: ReactPointerEvent<HTMLElement>) {
    const g = gesture.current;
    if (!g) return;
    if (!g.active) {
      const dist = Math.hypot(e.clientX - g.startX, e.clientY - g.startY);
      if (dist < DRAG_THRESHOLD_PX) return;
      g.active = true;
      setDragging(true);
      e.currentTarget.setPointerCapture(e.pointerId);
      // Hold the window interactive for the whole gesture (see module note).
      void setOverlayInteractive(true).catch(() => {});
    }
    const el = ref.current;
    const w = el?.offsetWidth ?? 0;
    const h = el?.offsetHeight ?? 0;
    setPos({
      x: Math.min(Math.max(e.clientX - g.dx, 0), Math.max(window.innerWidth - w, 0)),
      y: Math.min(Math.max(e.clientY - g.dy, 0), Math.max(window.innerHeight - h, 0)),
    });
  }

  function endGesture(e: ReactPointerEvent<HTMLElement>) {
    const g = gesture.current;
    gesture.current = null;
    if (!g?.active) return;
    setDragging(false);
    void setOverlayInteractive(false).catch(() => {});
    opts?.onDrop?.(e.clientX, e.clientY);
    if (opts?.resetOnDrop) setPos(null);
  }

  const style: CSSProperties | undefined = pos
    ? {
        left: pos.x,
        top: pos.y,
        right: "auto",
        bottom: "auto",
        transform: "none",
        margin: 0,
      }
    : undefined;

  return {
    style,
    dragging,
    handleProps: {
      onPointerDown,
      onPointerMove,
      onPointerUp: endGesture,
      onPointerCancel: endGesture,
    },
  };
}
