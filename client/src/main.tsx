import React from "react";
import { createRoot } from "react-dom/client";
import { App, MixerWindowApp, VideoEditorWindowApp, VideoMonitorApp } from "./App";
import "./styles.css";

const params = new URLSearchParams(window.location.search);
const isVideoEditor = params.get("videoEditor") === "1";
const isMixer = params.get("mixer") === "1";
const isVideoMonitor = params.get("videoMonitor") === "1";

createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {isVideoEditor ? <VideoEditorWindowApp />
      : isMixer ? <MixerWindowApp />
      : isVideoMonitor ? <VideoMonitorApp />
      : <App />}
  </React.StrictMode>
);
