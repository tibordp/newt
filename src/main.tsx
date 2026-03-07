import React, { Suspense, useEffect, useState } from "react";
import ReactDOM from "react-dom/client";

const MainWindow = React.lazy(() => import("./main_window/MainWindow"));
const Viewer = React.lazy(() => import("./viewer/Viewer"));
const Editor = React.lazy(() => import("./editor/Editor"));

import { createBrowserRouter, RouterProvider } from "react-router-dom";
import "./styles/globals.scss";
import { safeCommand } from "./lib/ipc";

// --- React app ---

const router = createBrowserRouter([
  {
    path: "/",
    element: (
      <Suspense>
        <MainWindow />
      </Suspense>
    ),
  },
  {
    path: "/viewer",
    element: (
      <Suspense>
        <Viewer />
      </Suspense>
    ),
  },
  {
    path: "/editor",
    element: (
      <Suspense>
        <Editor />
      </Suspense>
    ),
  },
]);

function App({ children }: { children: React.ReactNode }) {
  const [zoom, setZoom] = useState(1.0);
  const onkeydown = (e: KeyboardEvent) => {
    if (e.key == "=" && e.ctrlKey) {
      setZoom((z) => z * 1.1);
    } else if (e.key == "-" && e.ctrlKey) {
      setZoom((z) => z / 1.1);
    } else if (e.key == "0" && e.ctrlKey) {
      setZoom(1.0);
    } else {
      return;
    }

    e.preventDefault();
  };

  useEffect(() => {
    //document.body.style.zoom = zoom.toString();
    safeCommand("zoom", { factor: zoom });
  }, [zoom]);

  useEffect(() => {
    window.addEventListener("keydown", onkeydown);
    return () => window.removeEventListener("keydown", onkeydown);
  }, []);

  return <>{children}</>;
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  // <React.StrictMode>
  <App>
    <RouterProvider router={router} />
  </App>,
  // </React.StrictMode>
);
