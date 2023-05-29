import React from "react";

import ReactDOM from "react-dom/client";
import MainWindow from "./main_window/MainWindow";
import Viewer from "./viewer/Viewer";

import {
  createBrowserRouter,
  RouterProvider
} from "react-router-dom";
import "./styles.css";

const router = createBrowserRouter([
  {
    path: "/",
    element: <MainWindow />
  },
  {
    path: "/viewer",
    element: <Viewer />
  }
]);

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  // <React.StrictMode>
  <RouterProvider router={router} />
  // </React.StrictMode>
);
