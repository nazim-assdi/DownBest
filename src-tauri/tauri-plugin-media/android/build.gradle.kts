import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.downbest.media"
    compileSdk = 36

    defaultConfig {
        minSdk = 24
        consumerProguardFiles("consumer-rules.pro")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_11
        targetCompatibility = JavaVersion.VERSION_11
    }
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_11
    }
}

dependencies {
    implementation(project(":tauri-android"))
    implementation("androidx.core:core-ktx:1.13.1")

    // Bundled yt-dlp runtime (Python 3.13 + yt-dlp; no separate Python install).
    implementation("dev.ffmpegkit-maintained:yt-dlp-android:2.0.2")

    // LGPL FFmpeg runtime for on-device audio remux/conversion.
    implementation("dev.ffmpegkit-maintained:ffmpeg-kit-full:8.1.9")
}
