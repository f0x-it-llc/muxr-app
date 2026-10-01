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
    }
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

// Keep the shared frust checkout pristine AND keep this app's source tree
// clean: every Gradle build directory this project has — the root project's,
// `:app`'s, and the out-of-tree `:frust-embedding` module's — is redirected
// under THIS app's `build/android/` root. So two apps can build against one
// frust checkout without either polluting it, nothing is written under
// `android/build` or `android/app/build` any more, and one `frust clean`
// removes the lot. (Flutter's generated project does the same thing:
// `rootProject.buildDir = ../build`, every subproject under it.)
//
// `rootDir` is THIS `android/` directory, so `../build/android` is
// `<app>/build/android`. These three paths are the Gradle half of
// `frust-drive`'s `build_dirs::BuildLayout` (`android_root` / `android_app` /
// `android_embedding`) — that module is where the CLI looks for the outputs,
// so a change here without a change there loses the APK.
//
// Gradle's own per-project cache is NOT set here: it is written during
// configuration, before this block runs, so the Frust CLI passes
// `--project-cache-dir <app>/build/android/.gradle` on the `gradlew` command
// line instead. A hand-run `./gradlew` from `android/` gets the default
// `android/.gradle` — harmless, and `frust clean` removes it as a legacy path.
gradle.lifecycle.beforeProject {
    when (path) {
        ":" -> layout.buildDirectory.set(rootDir.resolve("../build/android"))
        ":app" -> layout.buildDirectory.set(rootDir.resolve("../build/android/app"))
        ":frust-embedding" ->
            layout.buildDirectory.set(rootDir.resolve("../build/android/frust-embedding"))
    }
}

// frust:plugin-includes — plugin-contributed `include(...)` lines go below.
// `frust plugin add` appends each plugin's Android library module here with
// the same include/projectDir/build-dir-redirect trio as `:frust-embedding`
// above, so every module this project ever gains — built-in or
// plugin-contributed — stays under THIS app's `build/android/` root.

// Added by `frust` Add Plugin: a plugin's Android library module,
// included by path out of the frust checkout (the same derivation
// `frust.embedding.dir` and the plugin's Cargo path dep use).
include(":frust-camera")
project(":frust-camera").projectDir = file("../../../frust/plugins/camera/platform/android")

gradle.lifecycle.beforeProject {
    if (path == ":frust-camera") {
        layout.buildDirectory.set(rootDir.resolve("../build/android/frust-camera"))
    }
}

// Added by `frust` Add Plugin: a plugin's Android library module,
// included by path out of the frust checkout (the same derivation
// `frust.embedding.dir` and the plugin's Cargo path dep use).
include(":frust-secure-storage")
project(":frust-secure-storage").projectDir = file("../../../frust/plugins/secure-storage/platform/android")

gradle.lifecycle.beforeProject {
    if (path == ":frust-secure-storage") {
        layout.buildDirectory.set(rootDir.resolve("../build/android/frust-secure-storage"))
    }
}

// Added by `frust` Add Plugin: a plugin's Android library module,
// included by path out of the frust checkout (the same derivation
// `frust.embedding.dir` and the plugin's Cargo path dep use).
include(":frust-iap")
project(":frust-iap").projectDir = file("../../../frust/plugins/iap/platform/android")

gradle.lifecycle.beforeProject {
    if (path == ":frust-iap") {
        layout.buildDirectory.set(rootDir.resolve("../build/android/frust-iap"))
    }
}