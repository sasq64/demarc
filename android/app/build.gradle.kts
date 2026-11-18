plugins {
    id("com.android.application")
}

android {
    namespace = "com.demarc.c64"
    compileSdk = 36

    // Without this AGP cannot find llvm-strip and packages the .so unstripped.
    ndkVersion = "29.0.14206865"

    defaultConfig {
        applicationId = "se.minnberg.demarc"
        // Matches the `-P` handed to cargo-ndk in scripts/run-android.sh.
        minSdk = 26
        targetSdk = 36
        // Play rejects an upload whose versionCode it has seen: -PversionCode=N.
        versionCode = project.findProperty("versionCode")?.toString()?.toInt() ?: 1
        versionName = "0.1.0"
        ndk {
            abiFilters += listOf(project.findProperty("abi")?.toString() ?: "arm64-v8a")
        }
    }

    // The Play upload key; its location and password live in ~/.gradle/gradle.properties.
    val uploadStore = project.findProperty("demarcUploadStoreFile")?.toString()
    signingConfigs {
        if (uploadStore != null) {
            create("upload") {
                storeFile = file(uploadStore)
                storePassword = project.property("demarcUploadStorePassword").toString()
                keyAlias = project.property("demarcUploadKeyAlias").toString()
                keyPassword = storePassword
            }
        }
    }

    buildTypes {
        debug {
            isJniDebuggable = true
        }
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.findByName("upload") ?: signingConfigs.getByName("debug")
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

// MiniActivity extends the platform NativeActivity: no AndroidX needed.
dependencies {
}
