import AppKit
import Foundation

enum AuthMethodKind: String, Decodable {
    case apiKey = "api_key"
    case browser = "browser"
    case none = "none"
}

enum AuthBillingMode: String, Decodable {
    case api
    case account
}

struct AuthMethodDescriptor: Identifiable, Hashable {
    let provider: KnownProvider
    let id: String
    let title: String
    let summary: String
    let buttonLabel: String
    let kind: AuthMethodKind
    let billingMode: AuthBillingMode
    let isExperimental: Bool
    let supportsCompletionCode: Bool
    let authMode: String

    var isAPIKey: Bool { kind == .apiKey }
    var isBrowserConnect: Bool { kind == .browser }

    var badgeText: String {
        switch billingMode {
        case .api:
            return "API Billing"
        case .account:
            return provider == .google ? "OAuth" : "Account Billing"
        }
    }

    var defaultProfileName: String {
        switch (provider, kind) {
        case (.openai, .browser):
            return "ChatGPT Account"
        case (.anthropic, .browser):
            return "Claude Account"
        case (.google, .browser):
            return "Google OAuth"
        default:
            return "\(provider.displayName) API Key"
        }
    }

    static var fallbackCatalog: [KnownProvider: [AuthMethodDescriptor]] {
        Dictionary(uniqueKeysWithValues: KnownProvider.allCases.map { provider in
            (provider, fallbackMethods(for: provider))
        })
    }

    static func fallbackMethods(for provider: KnownProvider) -> [AuthMethodDescriptor] {
        switch provider {
        case .openai:
            return [
                AuthMethodDescriptor(
                    provider: .openai,
                    id: "api_key",
                    title: "OpenAI API Key",
                    summary: "Direct Platform billing with an OpenAI API key.",
                    buttonLabel: "Add API Key",
                    kind: .apiKey,
                    billingMode: .api,
                    isExperimental: false,
                    supportsCompletionCode: false,
                    authMode: "api_key"
                ),
                AuthMethodDescriptor(
                    provider: .openai,
                    id: "openai_account",
                    title: "ChatGPT Account",
                    summary: "Experimental browser-based sign-in for account-backed usage.",
                    buttonLabel: "Connect ChatGPT Account",
                    kind: .browser,
                    billingMode: .account,
                    isExperimental: true,
                    supportsCompletionCode: false,
                    authMode: "openai_session"
                ),
            ]
        case .anthropic:
            return [
                AuthMethodDescriptor(
                    provider: .anthropic,
                    id: "api_key",
                    title: "Anthropic API Key",
                    summary: "Direct Anthropic Console billing with an API key.",
                    buttonLabel: "Add API Key",
                    kind: .apiKey,
                    billingMode: .api,
                    isExperimental: false,
                    supportsCompletionCode: false,
                    authMode: "api_key"
                ),
                AuthMethodDescriptor(
                    provider: .anthropic,
                    id: "anthropic_account",
                    title: "Claude Account",
                    summary: "Experimental browser-based sign-in for Claude account billing.",
                    buttonLabel: "Connect Claude Account",
                    kind: .browser,
                    billingMode: .account,
                    isExperimental: true,
                    supportsCompletionCode: false,
                    authMode: "anthropic_session"
                ),
            ]
        case .google:
            return [
                AuthMethodDescriptor(
                    provider: .google,
                    id: "api_key",
                    title: "Google API Key",
                    summary: "Direct Gemini billing with an AI Studio or Gemini API key.",
                    buttonLabel: "Add API Key",
                    kind: .apiKey,
                    billingMode: .api,
                    isExperimental: false,
                    supportsCompletionCode: false,
                    authMode: "api_key"
                ),
                AuthMethodDescriptor(
                    provider: .google,
                    id: "google_oauth",
                    title: "Google OAuth",
                    summary: "Browser-based OAuth flow for a Google account.",
                    buttonLabel: "Connect Google Account",
                    kind: .browser,
                    billingMode: .account,
                    isExperimental: false,
                    supportsCompletionCode: false,
                    authMode: "google_oauth"
                ),
            ]
        case .openrouter:
            return [
                AuthMethodDescriptor(
                    provider: .openrouter,
                    id: "api_key",
                    title: "OpenRouter API Key",
                    summary: "Direct OpenRouter billing with an API key.",
                    buttonLabel: "Add API Key",
                    kind: .apiKey,
                    billingMode: .api,
                    isExperimental: false,
                    supportsCompletionCode: false,
                    authMode: "api_key"
                ),
            ]
        case .kimi:
            return [
                AuthMethodDescriptor(
                    provider: .kimi,
                    id: "api_key",
                    title: "Kimi Code API Key",
                    summary: "Subscription key from the Kimi Code Console.",
                    buttonLabel: "Add API Key",
                    kind: .apiKey,
                    billingMode: .api,
                    isExperimental: false,
                    supportsCompletionCode: false,
                    authMode: "api_key"
                ),
            ]
        case .lmstudio:
            return []
        }
    }
}

struct BrowserAuthAttempt: Decodable, Identifiable {
    let id: String
    let provider: String?
    let method: String?
    let status: String
    let authorizeURL: URL?
    let instructions: String?
    let code: String?
    let requiresCompletionCode: Bool?
    let pollAfterMs: Int?
    let error: String?

    var isTerminal: Bool {
        let normalized = status.lowercased()
        return ["completed", "failed", "cancelled", "expired"].contains(normalized)
    }

    var statusText: String {
        status
            .replacingOccurrences(of: "_", with: " ")
            .capitalized
    }
}

@MainActor
final class BrowserAuthCoordinator: ObservableObject {
    @Published private(set) var catalog = AuthMethodDescriptor.fallbackCatalog
    @Published private(set) var catalogNote: String?
    @Published private(set) var selectedBrowserMethod: AuthMethodDescriptor?
    @Published var profileName = ""
    @Published var isDefault = false
    @Published var verificationCode = ""
    @Published var googleOAuthClientID = ""
    @Published var googleOAuthClientSecret = ""
    @Published var googleProjectID = ""
    @Published private(set) var activeAttempt: BrowserAuthAttempt?
    @Published private(set) var feedback: String?
    @Published private(set) var feedbackIsError = false
    @Published private(set) var isLoadingCatalog = false
    @Published private(set) var isStarting = false
    @Published private(set) var isRefreshing = false
    @Published private(set) var isCompleting = false

    private var didLoadCatalog = false

    var hasSelection: Bool {
        selectedBrowserMethod != nil
    }

    var isWorking: Bool {
        isLoadingCatalog || isStarting || isRefreshing || isCompleting
    }

    var canReopenBrowser: Bool {
        activeAttempt?.authorizeURL != nil
    }

    var shouldShowVerificationCodeField: Bool {
        !verificationCode.isEmpty || selectedBrowserMethod?.supportsCompletionCode == true || activeAttempt?.requiresCompletionCode == true
    }

    var shouldShowCompletionAction: Bool {
        activeAttempt != nil && shouldShowVerificationCodeField
    }

    var needsGoogleOAuthConfig: Bool {
        selectedBrowserMethod?.provider == .google
            && selectedBrowserMethod?.authMode == "google_oauth"
    }

    func methods(for provider: KnownProvider) -> [AuthMethodDescriptor] {
        catalog[provider] ?? AuthMethodDescriptor.fallbackMethods(for: provider)
    }

    func loadMethodCatalog(using model: AppModel) async {
        guard !didLoadCatalog else { return }

        isLoadingCatalog = true
        defer { isLoadingCatalog = false }

        do {
            catalog = try await model.fetchAuthMethodCatalog()
            catalogNote = nil
            didLoadCatalog = true
        } catch {
            catalog = AuthMethodDescriptor.fallbackCatalog
            catalogNote = "Using built-in provider actions until the gateway exposes /admin/auth-methods."
            didLoadCatalog = true
        }
    }

    func selectBrowserMethod(_ method: AuthMethodDescriptor) {
        selectedBrowserMethod = method
        profileName = method.defaultProfileName
        isDefault = false
        verificationCode = ""
        activeAttempt = nil
        feedback = nil
        feedbackIsError = false
    }

    func clearSelection() {
        selectedBrowserMethod = nil
        profileName = ""
        isDefault = false
        verificationCode = ""
        activeAttempt = nil
        feedback = nil
        feedbackIsError = false
    }

    func startSelectedMethod(using model: AppModel) async {
        guard let method = selectedBrowserMethod else { return }

        let trimmedName = profileName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedName.isEmpty else {
            feedback = "Profile name is required before starting browser sign-in."
            feedbackIsError = true
            return
        }

        isStarting = true
        defer { isStarting = false }

        do {
            let attempt = try await model.startAuthAttempt(
                provider: method.provider,
                methodID: method.id,
                profileName: trimmedName,
                isDefault: isDefault,
                metadata: startMetadata()
            )
            activeAttempt = attempt
            feedback = "Started \(method.title)."
            feedbackIsError = false
            if let url = attempt.authorizeURL {
                NSWorkspace.shared.open(url)
            }
            if attempt.isTerminal {
                await model.refresh()
            } else {
                await pollAttempt(using: model, attemptID: attempt.id, pollAfterMs: attempt.pollAfterMs)
            }
        } catch {
            feedback = AuthUIError.message(for: error)
            feedbackIsError = true
        }
    }

    func refreshActiveAttempt(using model: AppModel) async {
        guard let attempt = activeAttempt else { return }
        await loadAttempt(using: model, attemptID: attempt.id)
    }

    func completeActiveAttempt(using model: AppModel) async {
        guard let attempt = activeAttempt else { return }

        isCompleting = true
        defer { isCompleting = false }

        do {
            let response = try await model.completeAuthAttempt(
                id: attempt.id,
                verificationCode: verificationCode
            )
            if let response {
                activeAttempt = response
                if response.isTerminal {
                    await model.refresh()
                }
            } else {
                await loadAttempt(using: model, attemptID: attempt.id)
            }
            feedback = "Updated \(selectedBrowserMethod?.title ?? "browser sign-in") status."
            feedbackIsError = false
        } catch {
            feedback = AuthUIError.message(for: error)
            feedbackIsError = true
        }
    }

    func reopenBrowser() {
        guard let url = activeAttempt?.authorizeURL else { return }
        NSWorkspace.shared.open(url)
    }

    private func pollAttempt(using model: AppModel, attemptID: String, pollAfterMs: Int?) async {
        let intervalMs = max(pollAfterMs ?? 1500, 750)

        for _ in 0..<8 {
            guard !Task.isCancelled else { return }
            if activeAttempt?.isTerminal == true {
                return
            }

            try? await Task.sleep(nanoseconds: UInt64(intervalMs) * 1_000_000)
            await loadAttempt(using: model, attemptID: attemptID, suppressFeedback: true)

            if activeAttempt?.isTerminal == true {
                return
            }
        }
    }

    private func loadAttempt(using model: AppModel, attemptID: String, suppressFeedback: Bool = false) async {
        isRefreshing = true
        defer { isRefreshing = false }

        do {
            let attempt = try await model.fetchAuthAttempt(id: attemptID)
            activeAttempt = attempt
            if !suppressFeedback {
                feedback = "Refreshed sign-in status."
                feedbackIsError = false
            }
            if attempt.isTerminal {
                await model.refresh()
            }
        } catch {
            if !suppressFeedback {
                feedback = AuthUIError.message(for: error)
                feedbackIsError = true
            }
        }
    }

    private func startMetadata() -> [String: Any]? {
        guard needsGoogleOAuthConfig else { return nil }

        var metadata: [String: Any] = [:]
        let clientID = googleOAuthClientID.trimmingCharacters(in: .whitespacesAndNewlines)
        let clientSecret = googleOAuthClientSecret.trimmingCharacters(in: .whitespacesAndNewlines)
        let projectID = googleProjectID.trimmingCharacters(in: .whitespacesAndNewlines)

        if !clientID.isEmpty {
            metadata["oauth_client_id"] = clientID
        }
        if !clientSecret.isEmpty {
            metadata["oauth_client_secret"] = clientSecret
        }
        if !projectID.isEmpty {
            metadata["google_project_id"] = projectID
        }

        return metadata.isEmpty ? nil : metadata
    }
}

extension AppModel {
    func fetchAuthMethodCatalog() async throws -> [KnownProvider: [AuthMethodDescriptor]] {
        let envelope: AuthMethodsEnvelope = try await get(path: "/auth-methods")
        var mergedCatalog = AuthMethodDescriptor.fallbackCatalog

        for providerRecord in envelope.providers {
            guard let provider = KnownProvider(rawValue: providerRecord.provider) else { continue }
            let methods = providerRecord.methods.compactMap { payload in
                AuthMethodDescriptor(provider: provider, payload: payload)
            }
            if !methods.isEmpty {
                mergedCatalog[provider] = methods
            }
        }

        return mergedCatalog
    }

    func startAuthAttempt(
        provider: KnownProvider,
        methodID: String,
        profileName: String,
        isDefault: Bool,
        metadata: [String: Any]? = nil
    ) async throws -> BrowserAuthAttempt {
        var payload: [String: Any] = [
            "provider": provider.rawValue,
            "method": methodID,
            "name": profileName,
            "is_default": isDefault,
        ]
        if let metadata {
            payload["metadata"] = metadata
        }
        let data = try await post(path: "/auth/start", json: payload)
        return try AuthWireDecoder.decode(AuthAttemptEnvelope.self, from: data).attempt
    }

    func fetchAuthAttempt(id: String) async throws -> BrowserAuthAttempt {
        let envelope: AuthAttemptEnvelope = try await get(path: "/auth/attempts/\(id)")
        return envelope.attempt
    }

    func completeAuthAttempt(id: String, verificationCode: String) async throws -> BrowserAuthAttempt? {
        var payload: [String: Any] = [:]
        let trimmedCode = verificationCode.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmedCode.isEmpty {
            payload["verification_code"] = trimmedCode
        }

        let data = try await post(path: "/auth/attempts/\(id)/complete", json: payload)
        guard !AuthWireDecoder.isEmptyPayload(data) else {
            return nil
        }
        return try AuthWireDecoder.decode(AuthAttemptEnvelope.self, from: data).attempt
    }
}

private struct AuthMethodsEnvelope: Decodable {
    let providers: [AuthProviderMethodsRecord]
}

private struct AuthProviderMethodsRecord: Decodable {
    let provider: String
    let methods: [AuthMethodPayload]
}

private struct AuthMethodPayload: Decodable {
    let id: String
    let title: String?
    let summary: String?
    let description: String?
    let buttonLabel: String?
    let kind: AuthMethodKind?
    let billingMode: AuthBillingMode?
    let experimental: Bool?
    let supportsCompletionCode: Bool?
    let authMode: String?
}

private struct AuthAttemptEnvelope: Decodable {
    let attempt: BrowserAuthAttempt

    init(from decoder: Decoder) throws {
        if let attempt = try? BrowserAuthAttempt(from: decoder) {
            self.attempt = attempt
            return
        }

        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.attempt = try container.decode(BrowserAuthAttempt.self, forKey: .attempt)
    }

    private enum CodingKeys: String, CodingKey {
        case attempt
    }
}

private enum AuthWireDecoder {
    static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }()

    static func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
        try decoder.decode(T.self, from: data)
    }

    static func isEmptyPayload(_ data: Data) -> Bool {
        guard !data.isEmpty else { return true }
        guard let text = String(data: data, encoding: .utf8) else { return false }
        return text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
}

private extension AuthMethodDescriptor {
    init?(provider: KnownProvider, payload: AuthMethodPayload) {
        let fallback = AuthMethodDescriptor.fallbackMethods(for: provider)
        let fallbackMethod = fallback.first(where: { $0.id == payload.id })
        let kind = payload.kind ?? fallbackMethod?.kind ?? (payload.id == "api_key" ? .apiKey : .browser)
        let billingMode = payload.billingMode ?? fallbackMethod?.billingMode ?? (kind == .apiKey ? .api : .account)

        self.init(
            provider: provider,
            id: payload.id,
            title: payload.title ?? fallbackMethod?.title ?? payload.id.replacingOccurrences(of: "_", with: " ").capitalized,
            summary: payload.summary ?? payload.description ?? fallbackMethod?.summary ?? "Configure \(provider.displayName) authentication.",
            buttonLabel: payload.buttonLabel ?? fallbackMethod?.buttonLabel ?? "Connect",
            kind: kind,
            billingMode: billingMode,
            isExperimental: payload.experimental ?? fallbackMethod?.isExperimental ?? false,
            supportsCompletionCode: payload.supportsCompletionCode ?? fallbackMethod?.supportsCompletionCode ?? false,
            authMode: payload.authMode ?? fallbackMethod?.authMode ?? "api_key"
        )
    }
}

enum AuthUIError {
    static func local(_ message: String) -> NSError {
        NSError(domain: "AgentLlmMac.AuthUI", code: -1, userInfo: [NSLocalizedDescriptionKey: message])
    }

    static func message(for error: Error) -> String {
        let nsError = error as NSError
        if nsError.domain == "AgentLlmMac", nsError.code == 404 {
            return "Gateway auth flow endpoints are not wired yet."
        }
        let message = nsError.localizedDescription.trimmingCharacters(in: .whitespacesAndNewlines)
        return message.isEmpty ? "Something went wrong." : message
    }
}
