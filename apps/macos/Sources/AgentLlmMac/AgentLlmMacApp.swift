import AppKit
import SwiftUI

// MARK: - Snapshot harness plumbing

private struct SnapshotModeKey: EnvironmentKey {
    static let defaultValue = false
}

extension EnvironmentValues {
    var isSnapshot: Bool {
        get { self[SnapshotModeKey.self] }
        set { self[SnapshotModeKey.self] = newValue }
    }
}

// MARK: - Design tokens

enum Type {
    static let verdict = Font.system(size: 22, weight: .semibold)
    static let section = Font.system(size: 15, weight: .semibold)
    static let body = Font.system(size: 13)
    static let bodyStrong = Font.system(size: 13, weight: .semibold)
    static let label = Font.system(size: 11, weight: .semibold)
    static let data = Font.system(size: 13, design: .monospaced)
    static let dataSmall = Font.system(size: 11, design: .monospaced)
}

enum Space {
    static let xs: CGFloat = 4
    static let s: CGFloat = 8
    static let m: CGFloat = 12
    static let l: CGFloat = 20
    static let xl: CGFloat = 32
}

enum Signal {
    /// The one accent: a signal-lamp amber, tuned per scheme for contrast.
    static func accent(_ scheme: ColorScheme) -> Color {
        scheme == .dark
            ? Color(hue: 0.10, saturation: 0.80, brightness: 0.95)
            : Color(hue: 0.10, saturation: 0.95, brightness: 0.66)
    }
}

// MARK: - Route model

enum RouteState {
    case live(detail: String)
    case ready(detail: String)
    case missing(detail: String)
    case offline(detail: String)

    var lamp: Color {
        switch self {
        case .live: return .green
        case .ready: return Color(nsColor: .tertiaryLabelColor)
        case .missing: return .orange
        case .offline: return .red
        }
    }

    var detail: String {
        switch self {
        case let .live(detail), let .ready(detail), let .missing(detail), let .offline(detail):
            return detail
        }
    }
}

struct Route: Identifiable {
    let id: String
    let displayName: String
    let state: RouteState
    let modelCount: Int
    let localBaseURL: String
    let canAddKey: Bool
}

enum KnownProvider: String, CaseIterable {
    case lmstudio, kimi, anthropic, openai, google, openrouter

    var displayName: String {
        switch self {
        case .lmstudio: return "LM Studio"
        case .kimi: return "Kimi"
        case .anthropic: return "Anthropic"
        case .openai: return "OpenAI"
        case .google: return "Google"
        case .openrouter: return "OpenRouter"
        }
    }

    var authModes: [String] {
        switch self {
        case .openai: return ["api_key", "openai_session"]
        case .anthropic: return ["api_key", "anthropic_session"]
        case .google: return ["api_key", "google_oauth"]
        case .kimi, .openrouter: return ["api_key"]
        case .lmstudio: return []
        }
    }
}

// MARK: - API records

struct AdminStatus: Decodable {
    let service: String
    let version: String
    let host: String
    let port: Int
}

struct ProviderRecord: Decodable {
    let provider: String
    let displayName: String
    let upstreamBaseUrl: String
    let localBaseUrl: String
}

struct AuthProfile: Decodable, Identifiable {
    let id: Int64
    let provider: String
    let name: String
    let authMode: String
    let isDefault: Bool
}

struct ModelCacheEntry: Decodable {
    let id: Int64
    let provider: String
    let modelId: String
}

struct ProviderSummary: Decodable, Identifiable {
    let provider: ProviderRecord
    let authProfiles: [AuthProfile]
    let models: [ModelCacheEntry]

    var id: String { provider.provider }
}

struct ProjectRecord: Decodable, Identifiable {
    let id: Int64
    let name: String
    let projectKey: String
    let active: Bool
}

struct RequestLog: Decodable, Identifiable {
    let id: Int64
    let requestId: String
    let projectName: String
    let provider: String
    let method: String
    let path: String
    let statusCode: Int
    let latencyMs: Int
    let totalTokens: Int?
    let estimatedCostUsd: Double?
    let errorText: String?
    let createdAt: Date

    var isError: Bool { statusCode >= 400 || (errorText?.isEmpty == false) }

    var stampText: String {
        if Calendar.current.isDateInToday(createdAt) {
            return createdAt.formatted(date: .omitted, time: .standard)
        }
        return createdAt.formatted(.dateTime.month(.abbreviated).day())
    }

    var latencyText: String {
        switch latencyMs {
        case ..<1_000: return "\(latencyMs) ms"
        case ..<60_000: return String(format: "%.1f s", Double(latencyMs) / 1_000)
        default: return String(format: "%.1f m", Double(latencyMs) / 60_000)
        }
    }
}

struct ProvidersEnvelope: Decodable { let providers: [ProviderSummary] }
struct ProjectsEnvelope: Decodable { let projects: [ProjectRecord] }
struct RequestsEnvelope: Decodable { let requests: [RequestLog] }

// MARK: - Traffic aggregation

struct TrafficSummary {
    let requests: Int
    let tokens: Int
    let cost: Double
    let errors: Int

    var line: String {
        guard requests > 0 else { return "No traffic today" }
        var parts = ["\(requests) request\(requests == 1 ? "" : "s")"]
        if tokens > 0 { parts.append("\(tokens.abbreviated) tokens") }
        if cost > 0 { parts.append(String(format: "$%.2f est", cost)) }
        if errors > 0 { parts.append("\(errors) error\(errors == 1 ? "" : "s")") }
        return parts.joined(separator: "  ·  ")
    }
}

extension Int {
    var abbreviated: String {
        switch self {
        case 1_000_000...: return String(format: "%.1fM", Double(self) / 1_000_000)
        case 1_000...: return String(format: "%.1fk", Double(self) / 1_000)
        default: return "\(self)"
        }
    }
}

// MARK: - App model

@MainActor
final class AppModel: ObservableObject {
    @Published var adminStatus: AdminStatus?
    @Published var providers: [ProviderSummary] = []
    @Published var projects: [ProjectRecord] = []
    @Published var requests: [RequestLog] = []
    @Published var lastError: String?
    @Published var fetchedAt: Date?
    @Published var isRefreshing = false
    @Published var lmStudioModels: Int?
    @Published var keySheetProvider: KnownProvider?
    @Published var keyCopiedAt: Date?

    let adminBaseURL: URL
    private let decoder: JSONDecoder
    private var timer: Timer?

    init() {
        let rawURL = ProcessInfo.processInfo.environment["AGENT_LLM_ADMIN_URL"] ?? "http://127.0.0.1:8787/admin"
        self.adminBaseURL = URL(string: rawURL)!
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        decoder.dateDecodingStrategy = .iso8601
        self.decoder = decoder

        self.timer = Timer.scheduledTimer(withTimeInterval: 10, repeats: true) { [weak self] _ in
            guard let self else { return }
            Task { await self.refresh() }
        }
        self.timer?.tolerance = 1

        Task { await refresh() }
    }

    var gatewayBaseURL: URL {
        URL(string: adminBaseURL.absoluteString.replacingOccurrences(of: "/admin", with: "")) ?? adminBaseURL
    }

    var isOnline: Bool { fetchedAt != nil && lastError == nil }

    // MARK: Verdict

    var verdict: String {
        guard fetchedAt != nil else { return "Connecting…" }
        guard isOnline else { return "Gateway offline" }
        let open = routes.filter {
            if case .missing = $0.state { return false }
            if case .offline = $0.state { return false }
            return true
        }.count
        if lmStudioModels == nil {
            return "\(open) routes open · LM Studio server off"
        }
        return "\(open) of \(routes.count) routes open"
    }

    var verdictDetail: String? {
        guard isOnline else { return "Run bin/agent-llm-up, then refresh." }
        return nil
    }

    // MARK: Routes

    var routes: [Route] {
        KnownProvider.allCases.compactMap { known in
            guard let summary = providers.first(where: { $0.id == known.rawValue }) else { return nil }
            return route(for: known, summary: summary)
        }
    }

    private func route(for known: KnownProvider, summary: ProviderSummary) -> Route {
        let defaultProfile = summary.authProfiles.first(where: \.isDefault) ?? summary.authProfiles.first
        let recentlyActive = requests.contains {
            $0.provider == known.rawValue && Date().timeIntervalSince($0.createdAt) < 60
        }

        let state: RouteState
        switch known {
        case .lmstudio:
            if let count = lmStudioModels {
                state = recentlyActive
                    ? .live(detail: "local · \(count) models")
                    : .ready(detail: "local · \(count) models")
            } else {
                state = .offline(detail: "server off · lms server start")
            }
        default:
            if let profile = defaultProfile {
                let auth = profile.authMode == "api_key" ? "api key" : profile.authMode.replacingOccurrences(of: "_", with: " ")
                let detail = "\(profile.name) · \(auth)"
                state = recentlyActive ? .live(detail: detail) : .ready(detail: detail)
            } else {
                state = .missing(detail: "no key")
            }
        }

        return Route(
            id: known.rawValue,
            displayName: known.displayName,
            state: state,
            modelCount: known == .lmstudio ? (lmStudioModels ?? 0) : summary.models.count,
            localBaseURL: summary.provider.localBaseUrl,
            canAddKey: !known.authModes.isEmpty
        )
    }

    // MARK: Traffic

    var today: TrafficSummary {
        let calendar = Calendar.current
        let todays = requests.filter { calendar.isDateInToday($0.createdAt) }
        return TrafficSummary(
            requests: todays.count,
            tokens: todays.compactMap(\.totalTokens).reduce(0, +),
            cost: todays.compactMap(\.estimatedCostUsd).reduce(0, +),
            errors: todays.filter(\.isError).count
        )
    }

    var lastRequest: RequestLog? { requests.first }

    var hasRecentTraffic: Bool {
        guard let last = lastRequest else { return false }
        return Date().timeIntervalSince(last.createdAt) < 60
    }

    // MARK: Actions

    func refresh() async {
        guard !isRefreshing else { return }
        isRefreshing = true
        defer { isRefreshing = false }

        async let probe: Void = probeLmStudio()

        do {
            async let status: AdminStatus = get(path: "/status")
            async let providers: ProvidersEnvelope = get(path: "/providers")
            async let projects: ProjectsEnvelope = get(path: "/projects")
            async let requests: RequestsEnvelope = get(
                path: "/requests",
                queryItems: [URLQueryItem(name: "limit", value: "200")]
            )

            self.adminStatus = try await status
            self.providers = try await providers.providers
            self.projects = try await projects.projects
            self.requests = try await requests.requests
            self.fetchedAt = Date()
            self.lastError = nil
        } catch {
            self.lastError = error.localizedDescription
            self.fetchedAt = Date()
        }

        await probe
    }

    private func probeLmStudio() async {
        var request = URLRequest(url: URL(string: "http://127.0.0.1:1234/v1/models")!)
        request.timeoutInterval = 1.5
        if let (data, response) = try? await URLSession.shared.data(for: request),
           let http = response as? HTTPURLResponse, http.statusCode == 200,
           let body = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let models = body["data"] as? [[String: Any]] {
            lmStudioModels = models.count
        } else {
            lmStudioModels = nil
        }
    }

    func copyProjectKey() {
        guard let key = projects.first?.projectKey else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(key, forType: .string)
        keyCopiedAt = Date()
        Task {
            try? await Task.sleep(for: .seconds(2))
            if let copiedAt = keyCopiedAt, Date().timeIntervalSince(copiedAt) >= 2 {
                keyCopiedAt = nil
            }
        }
    }

    func submitKey(provider: KnownProvider, name: String, authMode: String, secret: String, isDefault: Bool) async throws {
        let payload: [String: Any] = [
            "provider": provider.rawValue,
            "name": name,
            "auth_mode": authMode,
            "secret": secret,
            "is_default": isDefault,
        ]
        _ = try await post(path: "/auth-profiles", json: payload)
        await refresh()
    }

    // MARK: HTTP

    func get<T: Decodable>(path: String, queryItems: [URLQueryItem] = []) async throws -> T {
        let url = try makeAdminURL(path: path, queryItems: queryItems)
        var request = URLRequest(url: url)
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        let (data, response) = try await URLSession.shared.data(for: request)
        try validate(response: response, data: data)
        return try decoder.decode(T.self, from: data)
    }

    func post(path: String, json: [String: Any]) async throws -> Data {
        let url = try makeAdminURL(path: path)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: json)
        let (data, response) = try await URLSession.shared.data(for: request)
        try validate(response: response, data: data)
        return data
    }

    private func makeAdminURL(path: String, queryItems: [URLQueryItem] = []) throws -> URL {
        guard var components = URLComponents(url: adminBaseURL, resolvingAgainstBaseURL: false) else {
            throw NSError(domain: "AgentLlmMac", code: -1, userInfo: [NSLocalizedDescriptionKey: "Invalid admin base URL."])
        }
        let trimmedPath = path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        var fullPath = components.path
        if !trimmedPath.isEmpty {
            if !fullPath.hasSuffix("/") { fullPath += "/" }
            fullPath += trimmedPath
        }
        components.path = fullPath
        components.queryItems = queryItems.isEmpty ? nil : queryItems
        guard let url = components.url else {
            throw NSError(domain: "AgentLlmMac", code: -1, userInfo: [NSLocalizedDescriptionKey: "Failed to build request URL."])
        }
        return url
    }

    private func validate(response: URLResponse, data: Data) throws {
        guard let http = response as? HTTPURLResponse else {
            throw NSError(domain: "AgentLlmMac", code: -1, userInfo: [NSLocalizedDescriptionKey: "Invalid server response."])
        }
        guard (200..<300).contains(http.statusCode) else {
            let text = String(data: data, encoding: .utf8) ?? "Unknown server error."
            throw NSError(domain: "AgentLlmMac", code: http.statusCode, userInfo: [NSLocalizedDescriptionKey: text])
        }
    }
}

// MARK: - App

enum WindowID {
    static let dashboard = "dashboard"
}

@main
struct AgentLlmMacApp: App {
    @StateObject private var model = AppModel()

    private var preview: String? { ProcessInfo.processInfo.environment["AGENT_LLM_PREVIEW"] }

    var body: some Scene {
        MenuBarExtra {
            MenuBarContent(model: model)
        } label: {
            Label("agent-llm", systemImage: model.isOnline ? "bolt.horizontal.circle.fill" : "bolt.horizontal.circle")
        }
        .menuBarExtraStyle(.window)

        Window("agent-llm", id: WindowID.dashboard) {
            DashboardView(model: model)
        }
        .defaultSize(width: 720, height: 640)
        .windowResizability(.contentSize)

        // Design-verification harness: AGENT_LLM_PREVIEW=popover|dashboard
        // presents that surface at launch; AGENT_LLM_SNAPSHOT=/path.png makes the
        // app write its own rendered window to disk (no screen-recording TCC
        // needed) and exit.
        if #available(macOS 15.0, *), preview != nil {
            Window("preview", id: "preview") {
                Group {
                    if preview == "popover" {
                        MenuBarContent(model: model)
                    } else {
                        DashboardView(model: model)
                    }
                }
                .onAppear { NSApplication.shared.activate(ignoringOtherApps: true) }
                .task { await snapshotIfRequested() }
            }
            .windowResizability(.contentSize)
            .defaultLaunchBehavior(.presented)
        }

        Settings {
            SettingsView(model: model)
        }
    }

    @MainActor
    private func snapshotIfRequested() async {
        guard let path = ProcessInfo.processInfo.environment["AGENT_LLM_SNAPSHOT"] else { return }
        try? await Task.sleep(for: .seconds(3))

        let scheme: ColorScheme =
            ProcessInfo.processInfo.environment["AGENT_LLM_APPEARANCE"] == "dark" ? .dark : .light
        let surface: AnyView = preview == "popover"
            ? AnyView(MenuBarContent(model: model).background(Color(nsColor: .windowBackgroundColor)))
            : AnyView(DashboardView(model: model))

        let renderer = ImageRenderer(
            content: surface
                .environment(\.colorScheme, scheme)
                .environment(\.isSnapshot, true)
        )
        renderer.scale = 2
        if let image = renderer.nsImage,
           let tiff = image.tiffRepresentation,
           let rep = NSBitmapImageRep(data: tiff),
           let data = rep.representation(using: .png, properties: [:]) {
            try? data.write(to: URL(fileURLWithPath: path))
        }
        NSApplication.shared.terminate(nil)
    }
}

// MARK: - Menu bar popover

struct MenuBarContent: View {
    @Environment(\.openWindow) private var openWindow
    @Environment(\.colorScheme) private var scheme
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: Space.xs) {
                HStack(spacing: Space.s) {
                    Lamp(color: model.isOnline ? .green : .red, active: model.hasRecentTraffic)
                    Text(model.verdict)
                        .font(Type.bodyStrong)
                }
                if let detail = model.verdictDetail {
                    Text(detail)
                        .font(Type.dataSmall)
                        .foregroundStyle(.secondary)
                        .padding(.leading, 16)
                } else {
                    Text(model.today.line)
                        .font(Type.dataSmall)
                        .foregroundStyle(.secondary)
                        .padding(.leading, 16)
                }
            }
            .padding(.horizontal, Space.m)
            .padding(.top, Space.m)
            .padding(.bottom, Space.m)

            Divider()

            VStack(spacing: 0) {
                ForEach(model.routes) { route in
                    HStack(spacing: Space.s) {
                        Lamp(color: route.state.lamp, active: false, size: 6)
                        Text(route.displayName)
                            .font(Type.body)
                        Spacer(minLength: Space.m)
                        Text(route.state.detail)
                            .font(Type.dataSmall)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    .padding(.horizontal, Space.m)
                    .padding(.vertical, 5)
                }
            }
            .padding(.vertical, Space.xs)

            if let last = model.lastRequest {
                Divider()
                HStack(spacing: Space.s) {
                    Text(last.provider)
                        .font(Type.dataSmall)
                    Text("\(last.statusCode)")
                        .font(Type.dataSmall)
                        .foregroundStyle(last.isError ? .red : .secondary)
                    Text(last.latencyText)
                        .font(Type.dataSmall)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Text(last.createdAt.formatted(.relative(presentation: .named)))
                        .font(Type.dataSmall)
                        .foregroundStyle(.secondary)
                }
                .padding(.horizontal, Space.m)
                .padding(.vertical, Space.s)
            }

            Divider()

            VStack(spacing: 0) {
                MenuActionRow(title: "Open dashboard", shortcut: "⌘D") {
                    openDashboard()
                }
                .keyboardShortcut("d")
                MenuActionRow(title: model.keyCopiedAt != nil ? "Copied" : "Copy project key", shortcut: nil) {
                    model.copyProjectKey()
                }
                MenuActionRow(title: "Quit", shortcut: "⌘Q") {
                    NSApplication.shared.terminate(nil)
                }
                .keyboardShortcut("q")
            }
            .padding(.vertical, Space.xs)
        }
        .frame(width: 300)
        .onAppear {
            Task { await model.refresh() }
        }
    }

    private func openDashboard() {
        openWindow(id: WindowID.dashboard)
        Task { @MainActor in
            NSApplication.shared.activate(ignoringOtherApps: true)
            try? await Task.sleep(for: .milliseconds(150))
            if let window = NSApplication.shared.windows.first(where: { $0.identifier?.rawValue == WindowID.dashboard }) {
                window.makeKeyAndOrderFront(nil)
            }
        }
    }
}

struct MenuActionRow: View {
    let title: String
    let shortcut: String?
    let action: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack {
                Text(title)
                    .font(Type.body)
                Spacer()
                if let shortcut {
                    Text(shortcut)
                        .font(Type.dataSmall)
                        .foregroundStyle(.tertiary)
                }
            }
            .padding(.horizontal, Space.m)
            .padding(.vertical, 5)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background(hovering ? AnyShapeStyle(.quaternary) : AnyShapeStyle(.clear))
        .onHover { hovering = $0 }
    }
}

// MARK: - Lamp

struct Lamp: View {
    let color: Color
    let active: Bool
    var size: CGFloat = 8
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var pulsing = false

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: size, height: size)
            .opacity(active && pulsing ? 0.5 : 1.0)
            .animation(
                active && !reduceMotion
                    ? .easeInOut(duration: 1.4).repeatForever(autoreverses: true)
                    : .default,
                value: pulsing
            )
            .onAppear { pulsing = active }
            .onChange(of: active) { _, newValue in pulsing = newValue }
    }
}

// MARK: - Dashboard

struct DashboardView: View {
    @ObservedObject var model: AppModel
    @Environment(\.colorScheme) private var scheme
    @Environment(\.isSnapshot) private var isSnapshot

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider()
            if isSnapshot {
                sections
            } else {
                ScrollView { sections }
            }
        }
        .frame(width: 720, height: 640)
        .background(Color(nsColor: .windowBackgroundColor))
        .sheet(item: $model.keySheetProvider) { provider in
            KeySheet(model: model, provider: provider)
        }
        .task { await model.refresh() }
    }

    private var sections: some View {
        VStack(alignment: .leading, spacing: Space.xl) {
            routesSection
            trafficSection
        }
        .padding(Space.l)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline, spacing: Space.m) {
            HStack(spacing: Space.s) {
                Image(systemName: "bolt.horizontal.circle.fill")
                    .foregroundStyle(Signal.accent(scheme))
                Text("agent-llm")
                    .font(Type.section)
            }
            Text(model.verdict)
                .font(Type.body)
                .foregroundStyle(.secondary)
            if let detail = model.verdictDetail {
                Text(detail)
                    .font(Type.dataSmall)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            Button(model.keyCopiedAt != nil ? "Copied" : "Copy project key") {
                model.copyProjectKey()
            }
            .buttonStyle(.borderless)
            .foregroundStyle(Signal.accent(scheme))
            Button {
                Task { await model.refresh() }
            } label: {
                Image(systemName: "arrow.clockwise")
            }
            .buttonStyle(.borderless)
            .keyboardShortcut("r")
            .help("Refresh")
        }
        .padding(.horizontal, Space.l)
        .padding(.vertical, Space.m)
    }

    private var routesSection: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            SectionLabel(text: "Routes")
            VStack(spacing: 0) {
                ForEach(Array(model.routes.enumerated()), id: \.element.id) { index, route in
                    if index > 0 { Divider() }
                    RouteRow(model: model, route: route)
                }
            }
        }
    }

    private var trafficSection: some View {
        VStack(alignment: .leading, spacing: Space.s) {
            SectionLabel(text: "Traffic")
            HStack(spacing: Space.s) {
                Text("Today")
                    .font(Type.bodyStrong)
                Text(model.today.line)
                    .font(Type.data)
                    .foregroundStyle(.secondary)
            }
            if model.requests.isEmpty {
                VStack(alignment: .leading, spacing: Space.xs) {
                    Text("Nothing has flowed yet.")
                        .font(Type.body)
                        .foregroundStyle(.secondary)
                    Text("Point a client at \(model.gatewayBaseURL.absoluteString) — recipes in docs/CODEX_SETUP.md.")
                        .font(Type.dataSmall)
                        .foregroundStyle(.tertiary)
                }
                .padding(.top, Space.s)
            } else {
                VStack(spacing: 0) {
                    ForEach(model.requests.prefix(12)) { request in
                        RequestRow(request: request)
                    }
                }
                .padding(.top, Space.xs)
            }
        }
    }
}

struct SectionLabel: View {
    let text: String

    var body: some View {
        Text(text.uppercased())
            .font(Type.label)
            .kerning(0.8)
            .foregroundStyle(.secondary)
    }
}

struct RouteRow: View {
    @ObservedObject var model: AppModel
    let route: Route
    @Environment(\.colorScheme) private var scheme
    @State private var hovering = false

    var body: some View {
        HStack(spacing: Space.m) {
            Lamp(color: route.state.lamp, active: false)
            Text(route.displayName)
                .font(Type.bodyStrong)
                .frame(width: 110, alignment: .leading)
            Text(route.state.detail)
                .font(Type.data)
                .foregroundStyle(.secondary)
                .lineLimit(1)
            Spacer()
            if route.canAddKey {
                Button(hasKey ? "Change key" : "Add key") {
                    model.keySheetProvider = KnownProvider(rawValue: route.id)
                }
                .buttonStyle(.borderless)
                .font(Type.body)
                .foregroundStyle(Signal.accent(scheme))
                .opacity(hasKey ? (hovering ? 1 : 0) : 1)
            }
        }
        .padding(.vertical, Space.s)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .contextMenu {
            Button("Copy base URL") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(route.localBaseURL, forType: .string)
            }
        }
    }

    private var hasKey: Bool {
        if case .missing = route.state { return false }
        return true
    }
}

struct RequestRow: View {
    let request: RequestLog

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: Space.m) {
                Text(request.stampText)
                    .foregroundStyle(.secondary)
                    .frame(width: 76, alignment: .leading)
                Text(request.path == "/v1/responses" ? "\(request.provider) · translate" : request.provider)
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .frame(width: 150, alignment: .leading)
                Text("\(request.statusCode)")
                    .foregroundStyle(request.isError ? AnyShapeStyle(Color.red) : AnyShapeStyle(.secondary))
                    .frame(width: 36, alignment: .trailing)
                Text(request.latencyText)
                    .foregroundStyle(.secondary)
                    .frame(width: 70, alignment: .trailing)
                Text(request.totalTokens.map { "\($0.abbreviated) tok" } ?? "—")
                    .foregroundStyle(.secondary)
                    .frame(width: 78, alignment: .trailing)
                Text(request.estimatedCostUsd.map { String(format: "$%.4f", $0) } ?? "")
                    .foregroundStyle(.tertiary)
                    .frame(width: 70, alignment: .trailing)
                Spacer()
            }
            if let error = request.errorText, !error.isEmpty {
                Text(error)
                    .foregroundStyle(.red)
                    .lineLimit(1)
                    .padding(.leading, 76 + 12)
            }
        }
        .font(Type.dataSmall)
        .monospacedDigit()
        .padding(.vertical, 3)
    }
}

// MARK: - Key sheet

extension KnownProvider: Identifiable {
    var id: String { rawValue }
}

struct KeySheet: View {
    @ObservedObject var model: AppModel
    let provider: KnownProvider
    @Environment(\.dismiss) private var dismiss
    @StateObject private var browserAuth = BrowserAuthCoordinator()

    private enum Path: Hashable {
        case apiKey
        case browser
    }

    @State private var path: Path = .apiKey
    @State private var name = "default-api"
    @State private var secret = ""
    @State private var isDefault = true
    @State private var errorText: String?
    @State private var saving = false

    private var browserMethod: AuthMethodDescriptor? {
        browserAuth.methods(for: provider).first(where: \.isBrowserConnect)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Space.l) {
            Text("\(provider.displayName) auth")
                .font(Type.section)

            if let method = browserMethod {
                Picker("", selection: $path) {
                    Text("API key").tag(Path.apiKey)
                    Text(method.title).tag(Path.browser)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
            }

            switch path {
            case .apiKey:
                apiKeyPane
            case .browser:
                browserPane
            }
        }
        .padding(Space.l)
        .frame(width: 440)
        .task {
            await browserAuth.loadMethodCatalog(using: model)
        }
    }

    private var apiKeyPane: some View {
        VStack(alignment: .leading, spacing: Space.m) {
            LabeledField(label: "Profile name") {
                TextField("", text: $name)
                    .textFieldStyle(.roundedBorder)
            }
            LabeledField(label: "Key") {
                SecureField("", text: $secret)
                    .textFieldStyle(.roundedBorder)
            }
            Toggle("Use as default for \(provider.displayName)", isOn: $isDefault)
                .font(Type.body)

            if let errorText {
                Text(errorText)
                    .font(Type.dataSmall)
                    .foregroundStyle(.red)
                    .lineLimit(3)
            }

            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button(saving ? "Saving…" : "Save key") { submit() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(saving)
            }
        }
    }

    private var browserPane: some View {
        VStack(alignment: .leading, spacing: Space.m) {
            LabeledField(label: "Profile name") {
                TextField("", text: $browserAuth.profileName)
                    .textFieldStyle(.roundedBorder)
            }
            Toggle("Use as default for \(provider.displayName)", isOn: $browserAuth.isDefault)
                .font(Type.body)

            if browserAuth.needsGoogleOAuthConfig {
                DisclosureGroup("OAuth client (optional)") {
                    VStack(alignment: .leading, spacing: Space.s) {
                        LabeledField(label: "Client ID") {
                            TextField("", text: $browserAuth.googleOAuthClientID)
                                .textFieldStyle(.roundedBorder)
                        }
                        LabeledField(label: "Client secret") {
                            SecureField("", text: $browserAuth.googleOAuthClientSecret)
                                .textFieldStyle(.roundedBorder)
                        }
                        LabeledField(label: "Project ID") {
                            TextField("", text: $browserAuth.googleProjectID)
                                .textFieldStyle(.roundedBorder)
                        }
                    }
                    .padding(.top, Space.s)
                }
                .font(Type.body)
            }

            if let attempt = browserAuth.activeAttempt {
                HStack(spacing: Space.s) {
                    Lamp(color: attempt.isTerminal ? (attempt.status.lowercased() == "completed" ? .green : .red) : .orange, active: false, size: 6)
                    Text(attempt.statusText)
                        .font(Type.body)
                    if browserAuth.canReopenBrowser {
                        Button("Reopen browser") { browserAuth.reopenBrowser() }
                            .buttonStyle(.link)
                            .font(Type.body)
                    }
                }
                if let instructions = attempt.instructions, !instructions.isEmpty {
                    Text(instructions)
                        .font(Type.dataSmall)
                        .foregroundStyle(.secondary)
                }
            }

            if browserAuth.shouldShowVerificationCodeField {
                LabeledField(label: "Verification code") {
                    TextField("", text: $browserAuth.verificationCode)
                        .textFieldStyle(.roundedBorder)
                        .font(Type.data)
                }
            }

            if let feedback = browserAuth.feedback {
                Text(feedback)
                    .font(Type.dataSmall)
                    .foregroundStyle(browserAuth.feedbackIsError ? AnyShapeStyle(Color.red) : AnyShapeStyle(.secondary))
                    .lineLimit(3)
            }

            HStack {
                Spacer()
                Button("Close") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                if browserAuth.activeAttempt == nil {
                    Button(browserAuth.isStarting ? "Opening…" : "Sign in with browser") {
                        Task { await browserAuth.startSelectedMethod(using: model) }
                    }
                    .keyboardShortcut(.defaultAction)
                    .disabled(browserAuth.isWorking)
                } else if browserAuth.shouldShowCompletionAction {
                    Button(browserAuth.isCompleting ? "Finishing…" : "Finish sign-in") {
                        Task { await browserAuth.completeActiveAttempt(using: model) }
                    }
                    .keyboardShortcut(.defaultAction)
                    .disabled(browserAuth.isWorking)
                } else {
                    Button(browserAuth.isRefreshing ? "Checking…" : "Check status") {
                        Task { await browserAuth.refreshActiveAttempt(using: model) }
                    }
                    .disabled(browserAuth.isWorking)
                }
            }
        }
        .onAppear {
            if let method = browserMethod, browserAuth.selectedBrowserMethod?.id != method.id {
                browserAuth.selectBrowserMethod(method)
                browserAuth.isDefault = true
            }
        }
    }

    private func submit() {
        let trimmedName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let trimmedSecret = secret.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty else {
            errorText = "Enter a profile name."
            return
        }
        guard !trimmedSecret.isEmpty else {
            errorText = "Paste the key — it stays in the local secret store."
            return
        }
        saving = true
        Task {
            do {
                try await model.submitKey(
                    provider: provider,
                    name: trimmedName,
                    authMode: "api_key",
                    secret: trimmedSecret,
                    isDefault: isDefault
                )
                dismiss()
            } catch {
                errorText = error.localizedDescription
            }
            saving = false
        }
    }
}

struct LabeledField<Content: View>: View {
    let label: String
    @ViewBuilder var content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: Space.xs) {
            Text(label)
                .font(Type.label)
                .foregroundStyle(.secondary)
            content
        }
    }
}

// MARK: - Settings

struct SettingsView: View {
    @ObservedObject var model: AppModel

    var body: some View {
        Form {
            LabeledContent("Gateway") {
                Text(model.gatewayBaseURL.absoluteString)
                    .font(Type.data)
                    .textSelection(.enabled)
            }
            LabeledContent("Admin API") {
                Text(model.adminBaseURL.absoluteString)
                    .font(Type.data)
                    .textSelection(.enabled)
            }
            LabeledContent("Responses API") {
                Text("\(model.gatewayBaseURL.absoluteString)/v1/responses")
                    .font(Type.data)
                    .textSelection(.enabled)
            }
            if let status = model.adminStatus {
                LabeledContent("Version") {
                    Text(status.version)
                        .font(Type.data)
                }
            }
        }
        .padding(Space.l)
        .frame(width: 460)
    }
}
