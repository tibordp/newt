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
