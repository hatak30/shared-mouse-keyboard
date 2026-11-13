plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

android {
    namespace = "io.github.hatak.sharedmousekeyboard"
    compileSdk = 36

    defaultConfig {
        applicationId = "io.github.hatak.sharedmousekeyboard"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = rootProject.file("../Cargo.toml").readLines()
            .first { it.trim().startsWith("version = ") }
            .substringAfter('"').substringBefore('"')
        ndk { abiFilters += listOf("arm64-v8a", "armeabi-v7a", "x86_64") }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    buildFeatures {
        compose = true
    }
}

// Builds libsmk_android.so for every ABI with cargo-ndk. Pass -PskipRust to
// reuse libraries that are already in src/main/jniLibs.
val cargoNdk = tasks.register<Exec>("cargoNdk") {
    group = "build"
    description = "Compile the Rust engine (crates/smk-android) for Android"
    enabled = !project.hasProperty("skipRust")
    workingDir = rootProject.projectDir.parentFile
    environment("PATH", "${System.getProperty("user.home")}/.cargo/bin:${System.getenv("PATH")}")
    commandLine(
        "cargo", "ndk",
        "-t", "arm64-v8a", "-t", "armeabi-v7a", "-t", "x86_64",
        "-o", layout.projectDirectory.dir("src/main/jniLibs").asFile.absolutePath,
        "build", "-p", "smk-android", "--release",
    )
}

tasks.configureEach {
    if (name.startsWith("merge") && name.endsWith("JniLibFolders")) dependsOn(cargoNdk)
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.activity.compose)
    implementation(libs.kotlinx.coroutines.android)
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.ui)
    implementation(libs.androidx.ui.tooling.preview)
    implementation(libs.androidx.material3)
    debugImplementation(libs.androidx.ui.tooling)
}
