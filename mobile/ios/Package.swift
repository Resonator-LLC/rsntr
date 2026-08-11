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
            path: "Sources/Resonator"
        ),
    ]
)
