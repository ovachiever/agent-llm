// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "AgentLlmMac",
    platforms: [
        .macOS(.v14),
    ],
    products: [
        .executable(
            name: "AgentLlmMac",
            targets: ["AgentLlmMac"]
        ),
    ],
    targets: [
        .executableTarget(
            name: "AgentLlmMac",
            path: "Sources/AgentLlmMac"
        ),
    ]
)
