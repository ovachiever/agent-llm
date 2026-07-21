const fs = require("node:fs");
const path = require("node:path");
const { app, BrowserWindow, Tray, Menu, nativeImage, ipcMain, shell, screen } = require("electron");

const DEFAULT_ADMIN_BASE_URL = process.env.AGENT_LLM_ADMIN_URL || "http://127.0.0.1:8787/admin";
const DEFAULT_API_BASE_URL = DEFAULT_ADMIN_BASE_URL.replace(/\/admin\/?$/, "");
const REQUEST_TIMEOUT_MS = 4000;

let mainWindow = null;
let tray = null;
app.isQuiting = false;
const windowStatePath = () => path.join(app.getPath("userData"), "window-state.json");
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

function createBaseIconSvg() {
  return `
    <svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256">
      <defs>
        <linearGradient id="bg" x1="0%" y1="0%" x2="100%" y2="100%">
          <stop offset="0%" stop-color="#0b1727"/>
          <stop offset="100%" stop-color="#12263f"/>
        </linearGradient>
        <linearGradient id="accent" x1="0%" y1="0%" x2="100%" y2="100%">
          <stop offset="0%" stop-color="#6ee7b7"/>
          <stop offset="100%" stop-color="#22c55e"/>
        </linearGradient>
      </defs>
      <rect x="18" y="18" width="220" height="220" rx="56" fill="url(#bg)"/>
      <path d="M74 73h53c36 0 58 21 58 54 0 34-22 56-58 56H74V73zm33 29v52h18c17 0 27-10 27-26 0-16-10-26-27-26h-18z" fill="#f8fafc"/>
      <circle cx="186" cy="75" r="18" fill="url(#accent)" opacity="0.92"/>
    </svg>
  `;
}

function createAppIcon() {
  const packagedIconPath = path.join(__dirname, "build", "icon.png");
  if (fs.existsSync(packagedIconPath)) {
    return nativeImage.createFromPath(packagedIconPath).resize({ width: 256, height: 256 });
  }

  const fallback = nativeImage.createFromDataURL(
    `data:image/svg+xml;base64,${Buffer.from(createBaseIconSvg()).toString("base64")}`
  );
  return fallback.resize({ width: 256, height: 256 });
}

function createTrayIcon() {
  const svg = `
    <svg xmlns="http://www.w3.org/2000/svg" width="18" height="18" viewBox="0 0 18 18">
      <rect x="1" y="1" width="16" height="16" rx="4" fill="#0f172a"/>
      <path d="M4.7 12.9V5.1h3c2.5 0 4 1.2 4 3.9s-1.5 3.9-4 3.9h-3zm1.8-1.5h1c1.4 0 2.3-.8 2.3-2.4 0-1.5-.9-2.4-2.3-2.4h-1v4.8z" fill="#f8fafc"/>
    </svg>
  `;

  const image = nativeImage.createFromDataURL(`data:image/svg+xml;base64,${Buffer.from(svg).toString("base64")}`);
  const resized = image.resize({ width: 18, height: 18 });
  if (process.platform === "darwin") {
    resized.setTemplateImage(true);
  }
  return resized;
}

function loadWindowState() {
  try {
    const raw = fs.readFileSync(windowStatePath(), "utf8");
    const parsed = JSON.parse(raw);
    if (
      parsed &&
      Number.isFinite(parsed.width) &&
      Number.isFinite(parsed.height) &&
      Number.isFinite(parsed.x) &&
      Number.isFinite(parsed.y)
    ) {
      const minWidth = 1120;
      const minHeight = 760;
      const display = screen.getDisplayNearestPoint({ x: parsed.x, y: parsed.y });
      const workArea = display?.workArea;
      const width = Math.max(minWidth, Math.min(parsed.width, workArea?.width || parsed.width));
      const height = Math.max(minHeight, Math.min(parsed.height, workArea?.height || parsed.height));
      const maxX = workArea ? workArea.x + workArea.width - width : parsed.x;
      const maxY = workArea ? workArea.y + workArea.height - height : parsed.y;
      const x = workArea ? Math.max(workArea.x, Math.min(parsed.x, maxX)) : parsed.x;
      const y = workArea ? Math.max(workArea.y, Math.min(parsed.y, maxY)) : parsed.y;
      return { width, height, x, y };
    }
  } catch (_error) {
    return null;
  }
  return null;
}

function saveWindowState() {
  if (!mainWindow || mainWindow.isDestroyed()) {
    return;
  }

  try {
    fs.writeFileSync(windowStatePath(), JSON.stringify(mainWindow.getBounds(), null, 2));
  } catch (_error) {
    // Non-fatal. The app should still function if window state persistence fails.
  }
}

function buildApplicationMenu() {
  const sendCommand = (payload) => {
    if (mainWindow && !mainWindow.isDestroyed()) {
      showWindow();
      mainWindow.webContents.send("agent-llm:command", payload);
    }
  };

  const template = [
    ...(process.platform === "darwin"
      ? [
          {
            label: app.name,
            submenu: [
              { role: "about" },
              { type: "separator" },
              { role: "services" },
              { type: "separator" },
              { role: "hide" },
              { role: "hideOthers" },
              { role: "unhide" },
              { type: "separator" },
              { role: "quit" },
            ],
          },
        ]
      : []),
    {
      label: "File",
      submenu: [
        {
          label: "New Auth Profile",
          accelerator: "CmdOrCtrl+N",
          click: () => sendCommand({ type: "show-view", view: "auth", focus: "profile-name" }),
        },
        { type: "separator" },
        {
          label: "Close Window",
          accelerator: "CmdOrCtrl+W",
          click: () => {
            if (mainWindow && !mainWindow.isDestroyed()) {
              mainWindow.hide();
            }
          },
        },
      ],
    },
    {
      label: "Edit",
      submenu: [
        { role: "undo" },
        { role: "redo" },
        { type: "separator" },
        { role: "cut" },
        { role: "copy" },
        { role: "paste" },
        { role: "selectAll" },
      ],
    },
    {
      label: "View",
      submenu: [
        {
          label: "Overview",
          accelerator: "CmdOrCtrl+1",
          click: () => sendCommand({ type: "show-view", view: "overview" }),
        },
        {
          label: "Auth Profiles",
          accelerator: "CmdOrCtrl+2",
          click: () => sendCommand({ type: "show-view", view: "auth", focus: "profile-name" }),
        },
        {
          label: "Activity",
          accelerator: "CmdOrCtrl+3",
          click: () => sendCommand({ type: "show-view", view: "activity" }),
        },
        { type: "separator" },
        {
          label: "Refresh",
          accelerator: "CmdOrCtrl+R",
          click: () => sendCommand({ type: "refresh" }),
        },
        {
          label: "Open API Base",
          accelerator: "CmdOrCtrl+Shift+O",
          click: () => sendCommand({ type: "open-api-base" }),
        },
        { type: "separator" },
        { role: "reload" },
        { role: "forceReload" },
        { type: "separator" },
        { role: "resetZoom" },
        { role: "zoomIn" },
        { role: "zoomOut" },
        { type: "separator" },
        { role: "togglefullscreen" },
      ],
    },
    {
      label: "Window",
      submenu: [{ role: "minimize" }, { role: "zoom" }, { role: "front" }],
    },
  ];

  Menu.setApplicationMenu(Menu.buildFromTemplate(template));
}

function createWindow() {
  const state = loadWindowState();
  mainWindow = new BrowserWindow({
    width: state?.width || 1240,
    height: state?.height || 860,
    x: state?.x,
    y: state?.y,
    minWidth: 1120,
    minHeight: 760,
    show: false,
    title: "agent-llm",
    backgroundColor: "#11161d",
    autoHideMenuBar: true,
    center: !state,
    resizable: true,
    movable: true,
    minimizable: true,
    maximizable: true,
    fullscreenable: true,
    titleBarStyle: process.platform === "darwin" ? "hiddenInset" : "default",
    icon: createAppIcon(),
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });

  mainWindow.loadFile(path.join(__dirname, "renderer", "index.html"));
  mainWindow.once("ready-to-show", () => {
    mainWindow.show();
  });

  mainWindow.on("close", (event) => {
    if (!app.isQuiting) {
      event.preventDefault();
      mainWindow.hide();
      updateTrayMenu();
    }
  });

  mainWindow.on("closed", () => {
    mainWindow = null;
    updateTrayMenu();
  });

  mainWindow.on("resize", saveWindowState);
  mainWindow.on("move", saveWindowState);
  mainWindow.on("show", updateTrayMenu);
  mainWindow.on("hide", updateTrayMenu);
}

function showWindow() {
  if (!mainWindow) {
    createWindow();
  }

  mainWindow.show();
  if (mainWindow.isMinimized()) {
    mainWindow.restore();
  }
  mainWindow.focus();
  updateTrayMenu();
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
      label: mainWindow && mainWindow.isVisible() ? "Hide Dashboard" : "Show Dashboard",
      click: () => {
        if (mainWindow && !mainWindow.isDestroyed() && mainWindow.isVisible()) {
          mainWindow.hide();
        } else {
          showWindow();
        }
      },
    },
    {
      label: "Close Window",
      click: () => {
        if (mainWindow && !mainWindow.isDestroyed()) {
          mainWindow.hide();
        }
      },
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
      label: "Quit Desktop App",
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
  app.setName("agent-llm");
  if (process.platform === "darwin" && app.dock) {
    app.dock.setIcon(createAppIcon());
  }
  app.setAboutPanelOptions({
    applicationName: "agent-llm",
    applicationVersion: "0.1.0",
    version: "0.1.0",
  });

  const gotSingleInstanceLock = app.requestSingleInstanceLock();
  if (!gotSingleInstanceLock) {
    app.quit();
    return;
  }

  app.on("second-instance", () => {
    showWindow();
  });

  buildApplicationMenu();
  createWindow();
  createTray();
  await fetchSnapshot();
});

app.on("before-quit", () => {
  app.isQuiting = true;
  if (tray) {
    tray.destroy();
    tray = null;
  }
});

app.on("window-all-closed", (event) => {
  if (!app.isQuiting) {
    event.preventDefault();
  }
});

app.on("activate", () => {
  showWindow();
});
