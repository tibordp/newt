import React, { Suspense, useEffect, useRef } from "react";
import ReactDOM from "react-dom/client";
import { ErrorBoundary, RouteErrorBoundary } from "./ErrorBoundary";

const MainWindow = React.lazy(() => import("./main_window/MainWindow"));
const Viewer = React.lazy(() => import("./viewer/Viewer"));
const Editor = React.lazy(() => import("./editor/Editor"));

import { createBrowserRouter, RouterProvider } from "react-router-dom";
import "./styles/globals.scss";
import { safe } from "./lib/ipc";
import { commands } from "./lib/bindings";
import { useRuntimeState } from "./lib/runtimeState";

const router = createBrowserRouter([
  {
    path: "/",
    errorElement: <RouteErrorBoundary />,
    element: (
      <Suspense>
        <MainWindow />
      </Suspense>
    ),
  },
  {
    path: "/viewer",
    errorElement: <RouteErrorBoundary />,
    element: (
      <Suspense>
        <Viewer />
      </Suspense>
    ),
  },
  {
    path: "/editor",
    errorElement: <RouteErrorBoundary />,
    element: (
      <Suspense>
        <Editor />
      </Suspense>
    ),
  },
]);

function App({ children }: { children: React.ReactNode }) {
  // Zoom is app-wide runtime state: every window applies the stored
  // factor, so it survives reloads and new windows start at it.
  const zoom = useRuntimeState()?.zoom ?? 1.0;
  const zoomRef = useRef(zoom);
  zoomRef.current = zoom;

  useEffect(() => {
    safe(commands.zoom(zoom));
  }, [zoom]);

  useEffect(() => {
    // Platform "mod": ⌘ on macOS, Ctrl elsewhere.
    const isMac = navigator.platform.startsWith("Mac");
    const onkeydown = (e: KeyboardEvent) => {
      const mod = isMac ? e.metaKey : e.ctrlKey;
      let next: number;
      if ((e.key == "=" || e.key == "+") && mod) {
        next = zoomRef.current * 1.1;
      } else if (e.key == "-" && mod) {
        next = zoomRef.current / 1.1;
      } else if (e.key == "0" && mod) {
        next = 1.0;
      } else {
        return;
      }

      e.preventDefault();
      next = Math.round(next * 10000) / 10000;
      // Apply immediately for latency; the broadcast re-applies the same
      // value everywhere (including here) once persisted.
      safe(commands.zoom(next));
      safe(commands.updateRuntimeState("zoom", next));
    };
    window.addEventListener("keydown", onkeydown);
    return () => window.removeEventListener("keydown", onkeydown);
  }, []);

  return <>{children}</>;
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  // <React.StrictMode>
  <ErrorBoundary>
    <App>
      <RouterProvider router={router} />
    </App>
  </ErrorBoundary>,
  // </React.StrictMode>
);
