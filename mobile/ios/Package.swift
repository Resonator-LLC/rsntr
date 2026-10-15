// swift-tools-version:5.9
// The resonator Swift package: the generated uniffi bindings compiled
// on top of the ResonatorFFI.xcframework that mobile/build-ios.sh packs
// under mobile/dist/. Run that script once before building this package
// (it also copies the generated resonator_ffi.swift into
// Sources/Resonator/).
import PackageDescription

let package = Package(
    name: "Resonator",
    // iOS only: the xcframework carries device + simulator slices.
    platforms: [
        .iOS(.v15)
    ],
    products: [
        .library(name: "Resonator", targets: ["Resonator"])
    ],
    targets: [
        .binaryTarget(
            name: "ResonatorFFI",
            path: "../dist/ResonatorFFI.xcframework"
        ),
        .target(
            name: "Resonator",
            dependencies: ["ResonatorFFI"],
            path: "Sources/Resonator",
            // The privacy manifest ships with the package so every app
            // that embeds it inherits the required-reason declarations.
            // App Store Connect checks those statically against the
            // linked binary, so the obligation arrives with the code
            // even in an app that links resonator and never starts a
            // node; without this each consumer meets it as an
            // ITMS-9105x mail after their first upload.
            resources: [
                .copy("PrivacyInfo.xcprivacy")
            ],
            // The Rust stack's system dependencies on iOS: iroh's DNS
            // discovery (hickory-resolver) calls SystemConfiguration,
            // and its interface enumeration (netdev) calls Network
            // (nw_endpoint_*, nw_interface_*). Both must be named here
            // or every consumer of this package fails at link time with
            // "Undefined symbols for architecture arm64" and has to
            // rediscover them (kontrolator iOS, 2026-09-10).
            linkerSettings: [
                .linkedFramework("SystemConfiguration"),
                .linkedFramework("Network")
            ]
        ),
    ]
)
