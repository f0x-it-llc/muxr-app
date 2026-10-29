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

// Machine-local directories — the `:frust-embedding` module below and every
// plugin module `frust plugin add` includes — never live in this tracked
// file. They sit in the gitignored `local.properties` beside it, written by
// `frust create` / `frust plugin add` and refreshed by every `frust run` /
// `frust build` from wherever cargo resolves each crate for this project, so
// they follow a `cargo update` or a move to another machine. A key that is
// missing fails here, naming the command that writes it.
val frustLocalPropertiesFile = rootDir.resolve("local.properties")
val frustLocalProperties = java.util.Properties().apply {
    if (frustLocalPropertiesFile.isFile) frustLocalPropertiesFile.inputStream().use { load(it) }
}

fun frustLocalDir(key: String): java.io.File {
    val dir = frustLocalProperties.getProperty(key)
        ?: throw org.gradle.api.GradleException(
            "`$key` is not set in $frustLocalPropertiesFile. Run `frust run` or `frust build` " +
                "for Android in the project directory: it writes the machine-local directory " +
                "of every Frust module there."
        )
    return frustLocalPropertiesFile.parentFile.resolve(dir)
}

// Frust's Android embedding (the Kotlin host: FrustActivity, FrustSurfaceView,
// FrustViewHost, the accesskit delegate) is a `com.android.library` module
// consumed by path out of the `frust-shell-android` crate cargo resolves for
// this project. It becomes a published Maven coordinate later.
include(":frust-embedding")
project(":frust-embedding").projectDir = frustLocalDir("frust.embedding.dir")

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
// above (its directory read through `frustLocalDir`, its key
// `frust.plugin.<module>.dir`), so every module this project ever gains —
// built-in or plugin-contributed — stays under THIS app's `build/android/`
// root.

// Added by `frust` Add Plugin: a plugin's Android library module,
// included from the plugin package cargo resolves for this app (its
// directory is machine-local: `frust.plugin.frust-camera.dir` in local.properties).
include(":frust-camera")
project(":frust-camera").projectDir = frustLocalDir("frust.plugin.frust-camera.dir")

gradle.lifecycle.beforeProject {
    if (path == ":frust-camera") {
        layout.buildDirectory.set(rootDir.resolve("../build/android/frust-camera"))
    }
}

// Added by `frust` Add Plugin: a plugin's Android library module,
// included from the plugin package cargo resolves for this app (its
// directory is machine-local: `frust.plugin.frust-secure-storage.dir` in local.properties).
include(":frust-secure-storage")
project(":frust-secure-storage").projectDir = frustLocalDir("frust.plugin.frust-secure-storage.dir")

gradle.lifecycle.beforeProject {
    if (path == ":frust-secure-storage") {
        layout.buildDirectory.set(rootDir.resolve("../build/android/frust-secure-storage"))
    }
}

// Added by `frust` Add Plugin: a plugin's Android library module,
// included from the plugin package cargo resolves for this app (its
// directory is machine-local: `frust.plugin.frust-iap.dir` in local.properties).
include(":frust-iap")
project(":frust-iap").projectDir = frustLocalDir("frust.plugin.frust-iap.dir")

gradle.lifecycle.beforeProject {
    if (path == ":frust-iap") {
        layout.buildDirectory.set(rootDir.resolve("../build/android/frust-iap"))
    }
}

// Added by `frust` Add Plugin: a plugin's Android library module,
// included from the plugin package cargo resolves for this app (its
// directory is machine-local: `frust.plugin.frust-connectivity.dir` in local.properties).
include(":frust-connectivity")
project(":frust-connectivity").projectDir = frustLocalDir("frust.plugin.frust-connectivity.dir")

gradle.lifecycle.beforeProject {
    if (path == ":frust-connectivity") {
        layout.buildDirectory.set(rootDir.resolve("../build/android/frust-connectivity"))
    }
}