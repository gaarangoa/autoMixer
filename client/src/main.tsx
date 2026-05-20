import React from "react";
import { createRoot } from "react-dom/client";
import { App, CameraPreviewApp } from "./App";
import "./styles.css";

const isCameraPreview = new URLSearchParams(window.location.search).get("cameraPreview") === "1";

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {isCameraPreview ? <CameraPreviewApp /> : <App />}
  </React.StrictMode>
);
