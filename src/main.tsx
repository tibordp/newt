import React, { useEffect, useState } from "react";
import ReactDOM from "react-dom/client";
import MainWindow from "./main_window/MainWindow";
import Viewer from "./viewer/Viewer";

import { invoke, Channel } from "@tauri-apps/api/core";
import { createBrowserRouter, RouterProvider } from "react-router-dom";
import "./styles.scss";
import { safeCommand } from "./lib/ipc";

// --- Splash screen init (runs before React mounts) ---

type InitEvent = { event: "status"; data: { message: string } };

const splashEl = document.getElementById("splash")!;
const splashStatus = document.getElementById("splash-status")!;
const splashError = document.getElementById("splash-error")!;

const onEvent = new Channel<InitEvent>();
onEvent.onmessage = (message) => {
  if (message.event === "status") {
    splashStatus.textContent = message.data.message;
  }
};

invoke("init", { onEvent })
  .then(() => {
    splashEl.style.display = "none";
  })
  .catch((err) => {
    splashStatus.style.display = "none";
    splashError.textContent = String(err);
  });

// --- React app ---

const router = createBrowserRouter([
  {
    path: "/",
    element: <MainWindow />,
  },
  {
    path: "/viewer",
    element: <Viewer />,
  },
]);

function App({ children }) {
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
  </App>
  // </React.StrictMode>
);
