import { Dialog, DialogPanel } from "@headlessui/react";
import { XMarkIcon } from "@heroicons/react/16/solid";
import { useId, type MouseEvent, type ReactNode, type RefObject } from "react";

import { useDraggablePopup, type PopupSize } from "./use-draggable-popup";

/**
 * How far the tabs stick out above the frame, at their tallest. The geometry keeps this
 * much room at the top of the screen and below the anchor. model-picker.css sets the tabs'
 * height to this on touch screens and less elsewhere.
 */
const TAB_OVERHANG = 32;

/** Six dots, the usual sign for "this can be dragged". */
function GripIcon() {
  return (
    <svg className="size-4" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
      <circle cx="3.5" cy="5.5" r="1.5" />
      <circle cx="8" cy="5.5" r="1.5" />
      <circle cx="12.5" cy="5.5" r="1.5" />
      <circle cx="3.5" cy="10.5" r="1.5" />
      <circle cx="8" cy="10.5" r="1.5" />
      <circle cx="12.5" cy="10.5" r="1.5" />
    </svg>
  );
}

/** A press on the empty boundary does nothing, not even taking focus out of the content. */
function ignoreBackgroundPress(event: MouseEvent<HTMLElement>) {
  if (event.target === event.currentTarget) event.preventDefault();
}

/**
 * A solid floating window with two tabs outside its top corners: a grip that moves it and
 * an X that closes it. Mount it only while it is open.
 *
 * Headless UI's Dialog owns the portal, focus containment, Escape and returning focus. Its
 * panel is a clear boundary over the whole screen with the window inside it, so a click on
 * the background lands inside the dialog: it neither closes this nor reaches the page
 * behind, whose selection and controls wait until the choice is made. Only the X and
 * Escape end it from here, and `onClose` is the one way out for both.
 *
 * The initial focus goes to whatever in `children` carries `data-autofocus`.
 */
export function DraggablePopup({
  label,
  moveLabel,
  closeLabel,
  anchorRef,
  preferredSize,
  onClose,
  children,
}: {
  label: string;
  moveLabel: string;
  closeLabel: string;
  anchorRef: RefObject<HTMLElement | null>;
  preferredSize: PopupSize;
  onClose: () => void;
  children: ReactNode;
}) {
  const hintId = useId();
  const popup = useDraggablePopup({ anchorRef, preferredSize, topOverhang: TAB_OVERHANG });

  return (
    <Dialog open onClose={onClose} aria-label={label} className="wfb-popup-layer">
      <DialogPanel className="wfb-popup-boundary" onMouseDown={ignoreBackgroundPress}>
        <div className="wfb-popup" data-dragging={popup.dragging || undefined} style={popup.style}>
          <button
            type="button"
            className="wfb-popup-tab wfb-popup-grip"
            aria-label={moveLabel}
            aria-describedby={hintId}
            {...popup.handleProps}
          >
            <GripIcon />
          </button>
          <span id={hintId} className="wfb-popup-grip-hint">
            Drag or use arrow keys. Home resets.
          </span>
          <button
            type="button"
            className="wfb-popup-tab wfb-popup-close"
            aria-label={closeLabel}
            title="Close"
            onClick={onClose}
          >
            <XMarkIcon className="size-4" aria-hidden="true" />
          </button>
          <div className="wfb-popup-content">{children}</div>
        </div>
      </DialogPanel>
    </Dialog>
  );
}
