const path = require("node:path");
const { app, BrowserWindow, Tray, Menu, nativeImage, ipcMain, shell } = require("electron");

const DEFAULT_ADMIN_BASE_URL = process.env.AGENT_LLM_ADMIN_URL || "http://127.0.0.1:8787/admin";
const DEFAULT_API_BASE_URL = DEFAULT_ADMIN_BASE_URL.replace(/\/admin\/?$/, "");
const REQUEST_TIMEOUT_MS = 4000;

let mainWindow = null;
let tray = null;
let lastSnapshot = {
  ok: false,
  baseUrl: DEFAULT_ADMIN_BASE_URL,
  fetchedAt: null,
  error: "Waiting for first refresh.",
  status: null,
  providers: [],
  projects: [],
  requests: [],
};

function createTrayIcon() {
  const svg = `
    <svg xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 18 18">
      <rect x="1" y="1" width="16" height="16" rx="4" fill="#111827"/>
      <path d="M5 13V5h3.2c2.4 0 3.8 1.2 3.8 3.3S10.6 13 8.2 13H5zm2-1.7h1c1.4 0 2.1-.9 2.1-2s-.7-2-2.1-2H7v4z" fill="#f8fafc"/>
    </svg>
  `;

  const image = nativeImage.createFromDataURL(`data:image/svg+xml;base64,${Buffer.from(svg).toString("base64")}`);
  return image.resize({ width: 18, height: 18 });
}

function createWindow() {
  mainWindow = new BrowserWindow({
    width: 1120,
    height: 760,
    minWidth: 920,
    minHeight: 620,
    show: false,
    title: "agent-llm",
    backgroundColor: "#0b1020",
    autoHideMenuBar: true,
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });

  mainWindow.loadFile(path.join(__dirname, "renderer", "index.html"));

  mainWindow.on("close", (event) => {
    if (!app.isQuiting) {
      event.preventDefault();
      mainWindow.hide();
    }
  });

  mainWindow.on("closed", () => {
    mainWindow = null;
  });
}

function showWindow() {
  if (!mainWindow) {
    createWindow();
  }

  mainWindow.show();
  mainWindow.focus();
}

function updateTrayMenu() {
  if (!tray) {
    return;
  }

  const healthLabel = lastSnapshot.ok
    ? `Gateway: online${lastSnapshot.status?.version ? ` (${lastSnapshot.status.version})` : ""}`
    : "Gateway: offline";

  const menu = Menu.buildFromTemplate([
    {
      label: healthLabel,
      enabled: false,
    },
    {
      type: "separator",
    },
    {
      label: "Open Dashboard",
      click: () => showWindow(),
    },
    {
      label: "Refresh",
      click: async () => {
        await fetchSnapshot();
        if (mainWindow && !mainWindow.isDestroyed()) {
          mainWindow.webContents.send("agent-llm:snapshot-updated", lastSnapshot);
        }
      },
    },
    {
      label: "Open API Base",
      click: () => shell.openExternal(DEFAULT_API_BASE_URL),
    },
    {
      type: "separator",
    },
    {
      label: "Quit",
      click: () => {
        app.isQuiting = true;
        app.quit();
      },
    },
  ]);

  tray.setContextMenu(menu);
  tray.setToolTip(healthLabel);
}

async function fetchJson(url) {
  return requestJson(url, { method: "GET" });
}

async function requestJson(url, init = {}) {
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);

  try {
    const method = init.method || "GET";
    const hasBody = Object.prototype.hasOwnProperty.call(init, "body");
    const response = await fetch(url, {
      method,
      signal: controller.signal,
      headers: {
        Accept: "application/json",
        ...(hasBody ? { "Content-Type": "application/json" } : {}),
        ...(init.headers || {}),
      },
      body: hasBody ? JSON.stringify(init.body) : undefined,
    });

    if (!response.ok) {
      const text = await response.text();
      throw new Error(`HTTP ${response.status}: ${text || response.statusText}`);
    }

    if (response.status === 204) {
      return null;
    }

    return await response.json();
  } finally {
    clearTimeout(timeoutId);
  }
}

async function fetchSnapshot() {
  const endpoints = {
    status: `${DEFAULT_ADMIN_BASE_URL}/status`,
    providers: `${DEFAULT_ADMIN_BASE_URL}/providers`,
    projects: `${DEFAULT_ADMIN_BASE_URL}/projects`,
    requests: `${DEFAULT_ADMIN_BASE_URL}/requests?limit=20`,
  };

  try {
    const [status, providers, projects, requests] = await Promise.all([
      fetchJson(endpoints.status),
      fetchJson(endpoints.providers),
      fetchJson(endpoints.projects),
      fetchJson(endpoints.requests),
    ]);

    lastSnapshot = {
      ok: true,
      baseUrl: DEFAULT_ADMIN_BASE_URL,
      fetchedAt: new Date().toISOString(),
      error: null,
      status,
      providers: Array.isArray(providers) ? providers : providers?.providers || providers?.items || [],
      projects: Array.isArray(projects) ? projects : projects?.projects || projects?.items || [],
      requests: Array.isArray(requests) ? requests : requests?.requests || requests?.items || [],
    };
  } catch (error) {
    lastSnapshot = {
      ok: false,
      baseUrl: DEFAULT_ADMIN_BASE_URL,
      fetchedAt: new Date().toISOString(),
      error: error instanceof Error ? error.message : "Unknown error",
      status: null,
      providers: [],
      projects: [],
      requests: [],
    };
  }

  updateTrayMenu();
  return lastSnapshot;
}

function createTray() {
  tray = new Tray(createTrayIcon());
  tray.on("click", () => showWindow());
  updateTrayMenu();
}

ipcMain.handle("agent-llm:get-snapshot", async () => {
  return fetchSnapshot();
});

ipcMain.handle("agent-llm:get-last-snapshot", async () => {
  return lastSnapshot;
});

ipcMain.handle("agent-llm:open-external", async (_event, url) => {
  if (typeof url === "string" && url.length > 0) {
    await shell.openExternal(url);
  }
});

ipcMain.handle("agent-llm:admin-request", async (_event, { path: requestPath, method, body }) => {
  const sanitizedPath = typeof requestPath === "string" ? requestPath : "/";
  const normalizedPath = sanitizedPath.startsWith("/") ? sanitizedPath : `/${sanitizedPath}`;
  const url = `${DEFAULT_ADMIN_BASE_URL}${normalizedPath}`;
  return requestJson(url, { method, body });
});

app.whenReady().then(async () => {
  const gotSingleInstanceLock = app.requestSingleInstanceLock();
  if (!gotSingleInstanceLock) {
    app.quit();
    return;
  }

  app.on("second-instance", () => {
    showWindow();
  });

  createWindow();
  createTray();
  await fetchSnapshot();
});

app.on("window-all-closed", (event) => {
  event.preventDefault();
});

app.on("activate", () => {
  showWindow();
});
