plugins {
    id("com.android.application")
}

android {
    namespace = "com.demarc.c64"
    compileSdk = 36

    // Without this AGP cannot find llvm-strip and packages the .so unstripped.
    ndkVersion = "29.0.14206865"

    defaultConfig {
        applicationId = "com.demarc.c64"
        // Matches the `-P` handed to cargo-ndk in scripts/run-android.sh.
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
        ndk {
            abiFilters += listOf("arm64-v8a")
        }
    }

    buildTypes {
        debug {
            isJniDebuggable = true
        }
        release {
            isMinifyEnabled = false
            // `adb install` rejects an unsigned APK, so sign release with the
            // keystore AGP generates for debug builds.
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    packaging {
        jniLibs {
            // Uncompressed, so the libretro core can be dlopen'd straight out
            // of the APK rather than extracted first.
            useLegacyPackaging = false
        }
    }
}

// android.app.NativeActivity is part of the platform: no AndroidX needed.
dependencies {
}
