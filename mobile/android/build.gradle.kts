// Resonator Android demo module. NOT compile-verified on this machine
// (no Android NDK was installed when it was scaffolded; see
// mobile/README.md). Build order:
//   1. mobile/build-android.sh   (needs an NDK; produces
//      src/main/jniLibs/arm64-v8a/libresonator_ffi.so and refreshes
//      mobile/kotlin/)
//   2. gradle assembleDebug      (from this directory, with an Android
//      SDK and AGP available)
plugins {
    id("com.android.application") version "8.5.0"
    id("org.jetbrains.kotlin.android") version "2.0.0"
}

android {
    namespace = "network.resonator.demo"
    compileSdk = 35

    defaultConfig {
        applicationId = "network.resonator.demo"
        // build-android.sh links against android api 24 (ANDROID_API).
        minSdk = 24
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
        ndk { abiFilters += "arm64-v8a" }
    }

    sourceSets {
        getByName("main") {
            // The generated uniffi bindings live beside this module and
            // are shared with any other JVM consumer.
            kotlin.srcDir("../kotlin")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }
}

dependencies {
    // The uniffi Kotlin bindings load the native library through JNA.
    implementation("net.java.dev.jna:jna:5.14.0@aar")
    implementation("androidx.appcompat:appcompat:1.7.0")
}
