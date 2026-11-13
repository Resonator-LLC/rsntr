// The Resonator app for Android: a WebView on the node that runs on the
// device (crates/resonator-ffi). build.sh puts the node library into
// src/main/jniLibs first.
import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

/** A setting from -P or the environment, else the default. */
fun setting(name: String, env: String, default: String): String =
    (project.findProperty(name) as String?) ?: System.getenv(env) ?: default

// Release signing: keystore.properties (local) or the environment (CI).
val signing = Properties().apply {
    val file = rootProject.file("keystore.properties")
    if (file.exists()) file.inputStream().use { load(it) }
}
fun secret(key: String, env: String): String? = signing.getProperty(key) ?: System.getenv(env)
val storeFilePath = secret("storeFile", "ANDROID_KEYSTORE_PATH")

android {
    namespace = "am.resonator.client.android"
    compileSdk = 35
    ndkVersion = "28.2.13676358"

    defaultConfig {
        applicationId = "am.resonator.client.android"
        minSdk = 26
        targetSdk = 35
        versionCode = setting("resonator.versionCode", "RESONATOR_VERSION_CODE", "400").toInt()
        versionName = setting("resonator.versionName", "RESONATOR_VERSION_NAME", "0.4.0")
        // The official network's relays; empty for iroh's public relays
        // and no push.
        buildConfigField("String", "IROH_RELAY",
            "\"" + setting("resonator.irohRelay", "RESONATOR_IROH_RELAY", "https://rly.rsntr.net") + "\"")
        buildConfigField("String", "PUSH_RELAY",
            "\"" + setting("resonator.pushRelay", "RESONATOR_PUSH_RELAY", "https://psh.rsntr.net") + "\"")
    }

    signingConfigs {
        if (storeFilePath != null) {
            create("release") {
                storeFile = file(storeFilePath)
                storePassword = secret("storePassword", "ANDROID_KEYSTORE_PASSWORD")
                keyAlias = secret("keyAlias", "ANDROID_KEY_ALIAS")
                keyPassword = secret("keyPassword", "ANDROID_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            if (storeFilePath != null) signingConfig = signingConfigs.getByName("release")
        }
    }

    buildFeatures { buildConfig = true }

    sourceSets {
        getByName("main") {
            // The generated UniFFI bindings (package am.resonator.client.ffi).
            java.srcDir("../../../crates/resonator-ffi/bindings")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions { jvmTarget = "17" }

    packaging {
        // Compressed: a much smaller download for the APK on Releases.
        jniLibs { useLegacyPackaging = true }
    }
}

dependencies {
    implementation("net.java.dev.jna:jna:5.15.0@aar")
}
