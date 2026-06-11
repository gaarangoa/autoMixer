import React from "react";
import { createRoot } from "react-dom/client";
import { App, CameraPreviewApp, MixerWindowApp, VideoEditorWindowApp } from "./App";
import "./styles.css";

const params = new URLSearchParams(window.location.search);
const isCameraPreview = params.get("cameraPreview") === "1";
const isVideoEditor = params.get("videoEditor") === "1";
const isMixer = params.get("mixer") === "1";

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {isCameraPreview ? <CameraPreviewApp /> : isVideoEditor ? <VideoEditorWindowApp /> : isMixer ? <MixerWindowApp /> : <App />}
  </React.StrictMode>
);
