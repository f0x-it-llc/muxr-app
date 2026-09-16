import groovy.json.JsonSlurper

pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
        // `rustls-platform-verifier`'s Android half (the
        // `org.rustls.platformverifier.CertificateVerifier` Kotlin class that
        // TrustConfig::SystemCa's JNI call needs) is not published to Maven
        // Central; the crate ships it only as a local Maven repo inside its own
        // cargo-registry checkout (crate README, "Deployment Considerations ->
        // Android"). Without this repository the class is absent from the APK
        // and every "Public domain (CA)" handshake fails at runtime with
        // `ClassNotFoundException: org.rustls.platformverifier.CertificateVerifier`.
        // This declaration MUST live here, in `dependencyResolutionManagement`,
        // and not in an app-level `repositories {}` block: Gradle's default
        // PREFER_PROJECT repository mode means any project-level repository
        // declaration makes Gradle ignore `google()`/`mavenCentral()` for that
        // project, and `:app` needs both for its other dependencies.
        maven {
            url = uri(rustlsPlatformVerifierMavenDir(settingsDir.parentFile))
            // `artifact()` alone can locate the module by scanning the
            // directory tree cargo built (no `maven-metadata.xml`, only cargo's
            // own `maven-metadata-local.xml`), but with no metadata read at all
            // Gradle has no way to know this module packages as an `.aar`
            // rather than a `.jar`, nor that a plain (non-SNAPSHOT) version
            // counts as a Maven "release" -- both of which the `latest.release`
            // dependency notation in `app/build.gradle.kts` needs. Adding
            // `mavenPom()` lets Gradle also read the POM cargo generates
            // alongside the AAR (packaging + implied release status) without
            // requiring Gradle Module Metadata, which this local repo doesn't
            // provide either.
            metadataSources { artifact(); mavenPom() }
        }
    }
}

// Locates rustls-platform-verifier's local Maven repo by asking cargo where the
// `rustls-platform-verifier-android` package (the crate's Kotlin/AAR half) lives
// on disk, rather than hardcoding a cargo-registry hash that changes with every
// crate release -- see the repository comment above for why this repo is needed
// at all. Fails Gradle configuration outright (never silently skips the
// repository) if cargo is unavailable, the exec fails, its output doesn't
// parse, or the package isn't found: a silent skip would reproduce the same
// ClassNotFoundException this repository exists to prevent.
fun rustlsPlatformVerifierMavenDir(muxrAppRoot: File): String {
    val manifestPath = try {
        val metadataJson = providers.exec {
            workingDir = muxrAppRoot
            commandLine(
                "cargo", "metadata",
                "--format-version", "1",
                "--filter-platform", "aarch64-linux-android",
                "--manifest-path", File(muxrAppRoot, "Cargo.toml").path,
            )
        }.standardOutput.asText.get()

        @Suppress("UNCHECKED_CAST")
        val packages = (JsonSlurper().parseText(metadataJson) as Map<String, Any?>)["packages"] as List<*>
        packages
            .map {
                @Suppress("UNCHECKED_CAST")
                it as Map<String, Any?>
            }
            .find { it["name"] == "rustls-platform-verifier-android" }
            ?.get("manifest_path") as String?
    } catch (e: Exception) {
        throw GradleException(
            "rustls-platform-verifier: could not run or parse `cargo metadata` while " +
                "locating the crate's Android component (crate README, 'Deployment " +
                "Considerations -> Android'). Is `cargo` on PATH? " + e.message,
            e,
        )
    } ?: throw GradleException(
        "rustls-platform-verifier: `cargo metadata` reported no " +
            "`rustls-platform-verifier-android` package -- the crate's Android " +
            "component (crate README, 'Deployment Considerations -> Android') could " +
            "not be located. Is rustls-platform-verifier still a dependency of this " +
            "workspace?",
    )

    return File(File(manifestPath).parentFile, "maven").path
}

rootProject.name = "muxr"
include(":app")

// Frust's Android embedding (the Kotlin host: FrustActivity, FrustSurfaceView,
// FrustViewHost, the accesskit delegate) is a `com.android.library` module
// consumed by path out of the frust checkout. `frust.embedding.dir` lives in
// `gradle.properties` — the single line to edit when this project moves to
// another machine. It becomes a published Maven coordinate post-crates.io.
include(":frust-embedding")
project(":frust-embedding").projectDir =
    file(providers.gradleProperty("frust.embedding.dir").get())

// Keep the shared frust checkout pristine: this project's build output goes
// under THIS app's tree, so two apps can build against one checkout.
gradle.lifecycle.beforeProject {
    if (path == ":frust-embedding") {
        layout.buildDirectory.set(rootDir.resolve("build/frust-embedding"))
    }
}

// frust:plugin-includes — plugin-contributed `include(...)` lines go below.

// Added by `frust` Add Plugin: a plugin's Android library module,
// included by path out of the frust checkout (the same derivation
// `frust.embedding.dir` and the plugin's Cargo path dep use).
include(":frust-camera")
project(":frust-camera").projectDir = file("../../../frust/plugins/camera/platform/android")

gradle.lifecycle.beforeProject {
    if (path == ":frust-camera") {
        layout.buildDirectory.set(rootDir.resolve("build/frust-camera"))
    }
}

// Added by `frust` Add Plugin: a plugin's Android library module,
// included by path out of the frust checkout (the same derivation
// `frust.embedding.dir` and the plugin's Cargo path dep use).
include(":frust-secure-storage")
project(":frust-secure-storage").projectDir = file("../../../frust/plugins/secure-storage/platform/android")

gradle.lifecycle.beforeProject {
    if (path == ":frust-secure-storage") {
        layout.buildDirectory.set(rootDir.resolve("build/frust-secure-storage"))
    }
}

// Added by `frust` Add Plugin: a plugin's Android library module,
// included by path out of the frust checkout (the same derivation
// `frust.embedding.dir` and the plugin's Cargo path dep use).
include(":frust-iap")
project(":frust-iap").projectDir = file("../../../frust/plugins/iap/platform/android")

gradle.lifecycle.beforeProject {
    if (path == ":frust-iap") {
        layout.buildDirectory.set(rootDir.resolve("build/frust-iap"))
    }
}