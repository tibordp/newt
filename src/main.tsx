import React from "react";

import ReactDOM from "react-dom/client";
import App from "./main_window/MainWindow";
import {
  createBrowserRouter,
  RouterProvider
} from "react-router-dom";
import "./styles.css";

const router = createBrowserRouter([
  {
    path: "/",
    element: <App />
  }
]);

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  // <React.StrictMode>
  <RouterProvider router={router} />
  // </React.StrictMode>
);
