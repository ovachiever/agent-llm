const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("agentLlm", {
  getSnapshot: () => ipcRenderer.invoke("agent-llm:get-snapshot"),
  getLastSnapshot: () => ipcRenderer.invoke("agent-llm:get-last-snapshot"),
  adminRequest: (request) => ipcRenderer.invoke("agent-llm:admin-request", request),
  openExternal: (url) => ipcRenderer.invoke("agent-llm:open-external", url),
  onSnapshotUpdated: (handler) => {
    const listener = (_event, payload) => handler(payload);
    ipcRenderer.on("agent-llm:snapshot-updated", listener);
    return () => ipcRenderer.removeListener("agent-llm:snapshot-updated", listener);
  },
  onCommand: (handler) => {
    const listener = (_event, payload) => handler(payload);
    ipcRenderer.on("agent-llm:command", listener);
    return () => ipcRenderer.removeListener("agent-llm:command", listener);
  },
});
