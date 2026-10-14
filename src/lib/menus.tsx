import { forwardRef } from "react";
import * as RadixContextMenu from "@radix-ui/react-context-menu";
import * as RadixDropdownMenu from "@radix-ui/react-dropdown-menu";

import { REFOCUS_EVENT } from "../main_window/types";

/// Hand focus back to whoever owns it — the active pane or terminal (see
/// CLAUDE.md, "Focus Management"). Radix would aim for whatever held focus
/// when the menu opened, which may have unmounted since.
export function refocusOwner(e: Event) {
  e.preventDefault();
  window.dispatchEvent(new Event(REFOCUS_EVENT));
}

// Whether the last pointer press asks for a context menu: a secondary
// button, or ctrl+click on macOS. The `contextmenu` event it causes
// consumes it.
let menuPress = false;
window.addEventListener(
  "pointerdown",
  (e) => {
    menuPress = e.button === 2 || (e.button === 0 && e.ctrlKey);
  },
  true,
);
const keyboardMenus = new WeakMap<Event, boolean>();
const replayed = new WeakSet<Event>();

/// Whether a `contextmenu` event came from the keyboard rather than a
/// pointer. The Menu key produces no keydown a view can bind, only this
/// event, and the webview aims it at a point of its own choosing — the
/// mouse pointer, a corner — so it must be recognised and re-aimed rather
/// than acted on where it landed. The button can't tell: macOS WebKit
/// reports the keyboard's as button 2. A pointer's request is the one that
/// follows a press asking for a menu. Answered once per event, so every
/// handler it passes hears the same.
export function isKeyboardContextMenu(e: MouseEvent): boolean {
  if (replayed.has(e)) return false;
  let keyboard = keyboardMenus.get(e);
  if (keyboard === undefined) {
    keyboard = !menuPress;
    menuPress = false;
    keyboardMenus.set(e, keyboard);
  }
  return keyboard;
}

/// Open the context menu `anchor` sits in, at the anchor's lower-left
/// corner, as a pointer's request would. Cancelable, so the menu's
/// `preventDefault` holds: WebKit builds its own menu from any
/// `contextmenu` that reaches its default handling, script-made or not.
export function openMenuAt(anchor: Element) {
  const rect = anchor.getBoundingClientRect();
  const event = new MouseEvent("contextmenu", {
    bubbles: true,
    cancelable: true,
    button: 2,
    buttons: 2,
    clientX: rect.left,
    clientY: rect.bottom,
  });
  replayed.add(event);
  anchor.dispatchEvent(event);
}

/// Handlers for the element around a view's context-menu trigger: a
/// keyboard request opens the menu at `anchor()` — the view's cursor — and
/// is caught in the capture phase, before the trigger would open it where
/// the webview aimed it; any other request no menu took, outside a text
/// field, is kept from the webview's own menu.
export function keyboardMenuHandlers(anchor: () => Element | null | undefined) {
  return {
    onContextMenuCapture: (e: React.MouseEvent) => {
      if (!isKeyboardContextMenu(e.nativeEvent)) return;
      e.preventDefault();
      e.stopPropagation();
      const at = anchor();
      if (at) openMenuAt(at);
    },
    onContextMenu: (e: React.MouseEvent) => {
      const target = e.target as HTMLElement;
      if (
        target instanceof HTMLInputElement ||
        target instanceof HTMLTextAreaElement ||
        target.isContentEditable
      ) {
        return;
      }
      e.preventDefault();
    },
  };
}

type ContextMenuContentProps = React.ComponentPropsWithoutRef<
  typeof RadixContextMenu.Content
>;
type DropdownMenuContentProps = React.ComponentPropsWithoutRef<
  typeof RadixDropdownMenu.Content
>;

const ContextMenuContent = forwardRef<HTMLDivElement, ContextMenuContentProps>(
  ({ onCloseAutoFocus, ...props }, ref) => (
    <RadixContextMenu.Content
      ref={ref}
      {...props}
      onCloseAutoFocus={onCloseAutoFocus ?? refocusOwner}
    />
  ),
);
ContextMenuContent.displayName = "ContextMenuContent";

const DropdownMenuContent = forwardRef<
  HTMLDivElement,
  DropdownMenuContentProps
>(({ onCloseAutoFocus, ...props }, ref) => (
  <RadixDropdownMenu.Content
    ref={ref}
    {...props}
    onCloseAutoFocus={onCloseAutoFocus ?? refocusOwner}
  />
));
DropdownMenuContent.displayName = "DropdownMenuContent";

/// Radix's menus, except that a menu closing returns focus to its owner
/// unless its `onCloseAutoFocus` says otherwise. The app imports menus from
/// here, never from Radix (enforced by ESLint), so a new menu can't strand
/// focus on `<body>` by forgetting to.
export const ContextMenu = { ...RadixContextMenu, Content: ContextMenuContent };
export const DropdownMenu = {
  ...RadixDropdownMenu,
  Content: DropdownMenuContent,
};
