// Top-level build file — plugin classpath only.
// AGP 9's built-in Kotlin support means no separate
// `org.jetbrains.kotlin.android` plugin is applied here or in `app/`.
//
// The `com.android.library` line is declared for explicitness only — AGP 9.3.0
// resolves the plugin for the included `:frust-embedding` module without it.
plugins {
    id("com.android.application") version "9.3.0" apply false
    id("com.android.library") version "9.3.0" apply false
}
