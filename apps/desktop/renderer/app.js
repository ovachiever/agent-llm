const POLL_INTERVAL_MS = 10000;

const AUTH_MODE_LABELS = {
  api_key: "API key",
  openai_session: "OpenAI session",
  anthropic_session: "Anthropic session",
};

const AUTH_MODE_OPTIONS = {
  openai: [
    { value: "api_key", label: AUTH_MODE_LABELS.api_key },
    { value: "openai_session", label: AUTH_MODE_LABELS.openai_session },
  ],
  anthropic: [
    { value: "api_key", label: AUTH_MODE_LABELS.api_key },
    { value: "anthropic_session", label: AUTH_MODE_LABELS.anthropic_session },
  ],
  google: [{ value: "api_key", label: AUTH_MODE_LABELS.api_key }],
  openrouter: [{ value: "api_key", label: AUTH_MODE_LABELS.api_key }],
};

const elements = {
  banner: document.getElementById("status-banner"),
  baseUrl: document.getElementById("base-url-value"),
  lastUpdated: document.getElementById("last-updated-value"),
  providersCount: document.getElementById("providers-count"),
  projectsCount: document.getElementById("projects-count"),
  requestsCount: document.getElementById("requests-count"),
  statusDetails: document.getElementById("status-details"),
  providersList: document.getElementById("providers-list"),
  projectsList: document.getElementById("projects-list"),
  requestsTableWrapper: document.getElementById("requests-table-wrapper"),
  refreshButton: document.getElementById("refresh-button"),
  openApiButton: document.getElementById("open-api-button"),
  profileForm: document.getElementById("profile-form"),
  profileProvider: document.getElementById("profile-provider"),
  profileAuthMode: document.getElementById("profile-auth-mode"),
  profileName: document.getElementById("profile-name"),
  profileSecret: document.getElementById("profile-secret"),
  profileHeadersJson: document.getElementById("profile-headers-json"),
  profileDefault: document.getElementById("profile-default"),
  profileFeedback: document.getElementById("profile-feedback"),
  profileModeHint: document.getElementById("profile-mode-hint"),
  profileSubmit: document.getElementById("profile-submit"),
};

function escapeHtml(value) {
  return String(value)
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

function formatTimestamp(value) {
  if (!value) {
    return "Unknown";
  }

  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }

  return date.toLocaleString();
}

function normalizeItems(payload) {
  if (Array.isArray(payload)) {
    return payload;
  }

  if (payload && Array.isArray(payload.providers)) {
    return payload.providers;
  }

  if (payload && Array.isArray(payload.projects)) {
    return payload.projects;
  }

  if (payload && Array.isArray(payload.requests)) {
    return payload.requests;
  }

  if (payload && Array.isArray(payload.items)) {
    return payload.items;
  }

  return [];
}

function setFeedback(message, tone = "") {
  elements.profileFeedback.className = "inline-message";
  if (tone) {
    elements.profileFeedback.classList.add(`inline-message-${tone}`);
  }
  elements.profileFeedback.textContent = message || "";
}

function providerDisplayName(provider) {
  return {
    openai: "OpenAI",
    anthropic: "Anthropic",
    google: "Google AI Studio",
    openrouter: "OpenRouter",
  }[provider] || provider;
}

function updateAuthModeOptions() {
  const provider = elements.profileProvider.value;
  const options = AUTH_MODE_OPTIONS[provider] || AUTH_MODE_OPTIONS.openai;
  const currentValue = elements.profileAuthMode.value;
  elements.profileAuthMode.innerHTML = options
    .map((option) => `<option value="${escapeHtml(option.value)}">${escapeHtml(option.label)}</option>`)
    .join("");

  if (options.some((option) => option.value === currentValue)) {
    elements.profileAuthMode.value = currentValue;
  }

  if (provider === "openai") {
    elements.profileModeHint.textContent =
      "OpenAI session profiles are for local session-backed billing on this Mac. API key profiles keep direct API billing.";
  } else if (provider === "anthropic") {
    elements.profileModeHint.textContent =
      "Anthropic session profiles are for your local Claude/Anthropic session-backed billing. Use extra headers JSON for beta flags when needed.";
  } else {
    elements.profileModeHint.textContent =
      `${providerDisplayName(provider)} currently expects API-key profiles in this dashboard.`;
  }
}

function parseHeadersMetadata() {
  const raw = elements.profileHeadersJson.value.trim();
  if (!raw) {
    return null;
  }

  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch (error) {
    throw new Error("Extra headers JSON must be valid JSON.");
  }

  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("Extra headers JSON must be an object.");
  }

  return { headers: parsed };
}

async function submitProfileForm(event) {
  event.preventDefault();
  setFeedback("");

  let metadata = null;
  try {
    metadata = parseHeadersMetadata();
  } catch (error) {
    setFeedback(error.message, "error");
    return;
  }

  const payload = {
    provider: elements.profileProvider.value,
    auth_mode: elements.profileAuthMode.value,
    name: elements.profileName.value.trim(),
    secret: elements.profileSecret.value.trim(),
    is_default: elements.profileDefault.checked,
    metadata,
  };

  if (!payload.name || !payload.secret) {
    setFeedback("Profile name and secret are required.", "error");
    return;
  }

  elements.profileSubmit.disabled = true;
  elements.profileSubmit.textContent = "Creating…";

  try {
    const response = await window.agentLlm.adminRequest({
      path: "/auth-profiles",
      method: "POST",
      body: payload,
    });
    const authProfile = response?.auth_profile;
    setFeedback(
      authProfile
        ? `Saved ${authProfile.name} for ${providerDisplayName(authProfile.provider)}.`
        : "Profile created.",
      "success"
    );
    elements.profileSecret.value = "";
    elements.profileHeadersJson.value = "";
    if (!elements.profileDefault.checked) {
      elements.profileName.value = "";
    }
    await refreshSnapshot();
  } catch (error) {
    setFeedback(error instanceof Error ? error.message : "Failed to create auth profile.", "error");
  } finally {
    elements.profileSubmit.disabled = false;
    elements.profileSubmit.textContent = "Create Profile";
  }
}

function renderBanner(snapshot) {
  elements.banner.className = "status-banner";

  if (!snapshot.fetchedAt) {
    elements.banner.classList.add("status-banner-pending");
    elements.banner.textContent = "Waiting for gateway status…";
    return;
  }

  if (snapshot.ok) {
    elements.banner.classList.add("status-banner-online");
    const providerCount = normalizeItems(snapshot.providers).length;
    const projectCount = normalizeItems(snapshot.projects).length;
    elements.banner.textContent = `Gateway online. Loaded ${providerCount} provider${providerCount === 1 ? "" : "s"} and ${projectCount} project${projectCount === 1 ? "" : "s"}.`;
    return;
  }

  elements.banner.classList.add("status-banner-offline");
  elements.banner.textContent = `Gateway offline. ${snapshot.error || "The admin API did not return a valid response."}`;
}

function renderStatusDetails(snapshot) {
  const status = snapshot.status;
  if (!snapshot.ok || !status || typeof status !== "object") {
    elements.statusDetails.innerHTML = `
      <div class="empty-state">
        The dashboard could not reach <span class="mono">${escapeHtml(snapshot.baseUrl)}</span>.
        Start the local gateway, then refresh this view.
      </div>
    `;
    return;
  }

  const entries = Object.entries(status);
  if (entries.length === 0) {
    elements.statusDetails.innerHTML = `<div class="empty-state">Gateway responded, but no status fields were returned.</div>`;
    return;
  }

  elements.statusDetails.innerHTML = entries
    .map(([key, value]) => {
      const renderedValue =
        value && typeof value === "object" ? escapeHtml(JSON.stringify(value)) : escapeHtml(value ?? "null");
      return `
        <div>
          <dt>${escapeHtml(key)}</dt>
          <dd class="${typeof value === "string" && value.length > 18 ? "mono" : ""}">${renderedValue}</dd>
        </div>
      `;
    })
    .join("");
}

function renderCollection(container, items, emptyText, renderItem) {
  if (items.length === 0) {
    container.innerHTML = `<div class="empty-state">${escapeHtml(emptyText)}</div>`;
    return;
  }

  container.innerHTML = items.map(renderItem).join("");
}

function renderProviders(snapshot) {
  const providers = normalizeItems(snapshot.providers);
  renderCollection(
    elements.providersList,
    providers,
    snapshot.ok ? "No providers are configured yet." : "Provider information is unavailable while the gateway is offline.",
    (provider) => {
      const record = provider.provider || provider;
      const name = record.display_name || record.name || record.provider || "Unnamed provider";
      const baseUrl = record.local_base_url || record.base_url || record.endpoint || "No base URL available";
      const authProfiles = Array.isArray(provider.auth_profiles) ? provider.auth_profiles : [];
      const defaultProfile = authProfiles.find((profile) => profile.is_default);
      const mode = defaultProfile
        ? `${defaultProfile.name} (${AUTH_MODE_LABELS[defaultProfile.auth_mode] || defaultProfile.auth_mode})`
        : "no default auth";
      const enabled = true;
      const modelCount = Array.isArray(provider.models) ? provider.models.length : 0;
      const profileSummary = authProfiles.length
        ? authProfiles
            .slice(0, 3)
            .map((profile) => {
              const label = AUTH_MODE_LABELS[profile.auth_mode] || profile.auth_mode || "Unknown";
              return `${profile.name}${profile.is_default ? " [default]" : ""} - ${label}`;
            })
            .join(" | ")
        : "No auth profiles configured yet";

      return `
        <article class="list-item">
          <strong>${escapeHtml(name)}</strong>
          <span class="mono">${escapeHtml(baseUrl)}</span>
          <span>${escapeHtml(String(authProfiles.length))} auth profile(s), ${escapeHtml(String(modelCount))} cached model(s)</span>
          <span>${escapeHtml(profileSummary)}</span>
          <div class="pill-row">
            <span class="pill ${enabled ? "pill-success" : "pill-warning"}">${enabled ? "enabled" : "disabled"}</span>
            <span class="pill">${escapeHtml(mode)}</span>
          </div>
        </article>
      `;
    }
  );
}

function renderProjects(snapshot) {
  const projects = normalizeItems(snapshot.projects);
  renderCollection(
    elements.projectsList,
    projects,
    snapshot.ok ? "No projects are linked yet." : "Project information is unavailable while the gateway is offline.",
    (project) => {
      const name = project.name || project.slug || project.id || "Unnamed project";
      const mode = project.active === false ? "inactive" : "active";
      const provider = project.default_provider || project.provider || "per-provider settings";
      const model = project.default_model || project.model || "set via CLI/admin";

      return `
        <article class="list-item">
          <strong>${escapeHtml(name)}</strong>
          <span>Provider: ${escapeHtml(provider)}</span>
          <span>Model: ${escapeHtml(model)}</span>
          <div class="pill-row">
            <span class="pill">${escapeHtml(mode)}</span>
          </div>
        </article>
      `;
    }
  );
}

function renderRequests(snapshot) {
  const requests = normalizeItems(snapshot.requests);
  if (requests.length === 0) {
    elements.requestsTableWrapper.innerHTML = `<div class="empty-state">${escapeHtml(
      snapshot.ok ? "No recent requests have been recorded yet." : "Recent request data is unavailable while the gateway is offline."
    )}</div>`;
    return;
  }

  const rows = requests
    .map((request) => {
      const startedAt = request.started_at || request.created_at || request.timestamp || null;
      const project = request.project_name || request.project_id || "Unknown";
      const provider = request.provider || request.provider_name || "Unknown";
      const path = request.path || request.route || request.endpoint || "Unknown";
      const status = request.status || request.status_code || "n/a";
      const latency = request.latency_ms || request.duration_ms || "n/a";
      const authProfile = request.auth_profile_name || "default";
      const cost = request.estimated_cost_usd != null ? `$${Number(request.estimated_cost_usd).toFixed(4)}` : "n/a";

      return `
        <tr>
          <td>${escapeHtml(formatTimestamp(startedAt))}</td>
          <td>${escapeHtml(project)}</td>
          <td>${escapeHtml(provider)}</td>
          <td>${escapeHtml(authProfile)}</td>
          <td class="mono">${escapeHtml(path)}</td>
          <td>${escapeHtml(status)}</td>
          <td>${escapeHtml(latency)}</td>
          <td>${escapeHtml(cost)}</td>
        </tr>
      `;
    })
    .join("");

  elements.requestsTableWrapper.innerHTML = `
    <table>
      <thead>
        <tr>
          <th>Started</th>
          <th>Project</th>
          <th>Provider</th>
          <th>Profile</th>
          <th>Path</th>
          <th>Status</th>
          <th>Latency ms</th>
          <th>Cost</th>
        </tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>
  `;
}

function renderSnapshot(snapshot) {
  renderBanner(snapshot);
  elements.baseUrl.textContent = snapshot.baseUrl || "Unknown";
  elements.lastUpdated.textContent = snapshot.fetchedAt
    ? `Last refresh: ${formatTimestamp(snapshot.fetchedAt)}`
    : "No successful refresh yet.";
  elements.providersCount.textContent = String(normalizeItems(snapshot.providers).length);
  elements.projectsCount.textContent = String(normalizeItems(snapshot.projects).length);
  elements.requestsCount.textContent = String(normalizeItems(snapshot.requests).length);

  renderStatusDetails(snapshot);
  renderProviders(snapshot);
  renderProjects(snapshot);
  renderRequests(snapshot);
}

async function refreshSnapshot() {
  elements.refreshButton.disabled = true;
  elements.refreshButton.textContent = "Refreshing…";

  try {
    const snapshot = await window.agentLlm.getSnapshot();
    renderSnapshot(snapshot);
  } finally {
    elements.refreshButton.disabled = false;
    elements.refreshButton.textContent = "Refresh";
  }
}

async function bootstrap() {
  updateAuthModeOptions();

  elements.refreshButton.addEventListener("click", () => {
    refreshSnapshot();
  });

  elements.profileProvider.addEventListener("change", () => {
    updateAuthModeOptions();
    setFeedback("");
  });

  elements.profileForm.addEventListener("submit", submitProfileForm);

  elements.openApiButton.addEventListener("click", async () => {
    const snapshot = await window.agentLlm.getLastSnapshot();
    await window.agentLlm.openExternal((snapshot.baseUrl || "").replace(/\/admin\/?$/, ""));
  });

  const initialSnapshot = await window.agentLlm.getLastSnapshot();
  renderSnapshot(initialSnapshot);
  await refreshSnapshot();

  window.agentLlm.onSnapshotUpdated((snapshot) => {
    renderSnapshot(snapshot);
  });

  window.setInterval(() => {
    refreshSnapshot();
  }, POLL_INTERVAL_MS);
}

bootstrap();
