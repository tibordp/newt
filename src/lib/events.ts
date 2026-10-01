/// Whether `e` happened inside `root` in the page. React bubbles an event
/// up the component tree it was rendered from, portals included, so a
/// handler on a container also hears events from the menus and popovers
/// it renders, which the page holds elsewhere.
export function ownsEvent(
  root: Element | null | undefined,
  e: { target: EventTarget | null },
): boolean {
  return !!root && e.target instanceof Node && root.contains(e.target);
}
