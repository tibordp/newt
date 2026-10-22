import { useEffect } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

import { setFullscreen, useAppearance } from "./appearance";
import styles from "./TitleBar.module.scss";

/// macOS draws the window's title and traffic lights over the top of the
/// page; this is the strip they sit on, which also moves and zooms the
/// window. Window chrome is the one place a component follows the
/// platform: the other systems draw their own title bars.
export function TitleBar() {
  const { platform } = useAppearance();
  const macos = platform === "macos";

  useEffect(() => {
    if (!macos) return;
    const win = getCurrentWindow();
    const sync = () => win.isFullscreen().then(setFullscreen, () => {});
    sync();
    const unlisten = win.onResized(sync);
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [macos]);

  return macos ? (
    <div className={styles.titleBar} data-tauri-drag-region />
  ) : null;
}
