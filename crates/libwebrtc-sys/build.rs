use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let manifest_path = PathBuf::from(&manifest_dir);

    // WebRTC directory (relative to workspace root)
    let webrtc_dir = manifest_path
        .join("../../deps/webrtc/macos_arm64")
        .canonicalize()
        .expect("WebRTC directory not found. Run 'make download' first.");

    let include_dir = webrtc_dir.join("include");
    let lib_dir = webrtc_dir.join("lib");

    // Our custom include directory with __config_site override
    let custom_include = manifest_path.join("wrapper/include");

    println!("cargo:rerun-if-changed=wrapper/wrapper.h");
    println!("cargo:rerun-if-changed=wrapper/wrapper.cpp");
    println!("cargo:rerun-if-changed=wrapper/include/__config_site");

    // Compile wrapper.cpp
    //
    // libwebrtc.a uses libc++ with __Cr namespace (Chromium ABI).
    // We override __config_site to use the same ABI namespace.
    //
    // The key is to include our custom __config_site BEFORE the system one.
    // This is done by adding our include directory with -isystem FIRST.
    cc::Build::new()
        .cpp(true)
        .file("wrapper/wrapper.cpp")
        // CRITICAL: Our custom include dir must come FIRST to override __config_site
        .flag(&format!("-isystem{}", custom_include.display()))
        // WebRTC headers
        .include(&include_dir)
        .include(include_dir.join("third_party/abseil-cpp"))
        .include(include_dir.join("third_party/libyuv/include"))
        .flag("-std=c++17")
        .flag("-stdlib=libc++")
        .flag("-fno-rtti")
        .flag("-Wno-deprecated-declarations")
        .flag("-Wno-unused-parameter")
        .define("WEBRTC_MAC", None)
        .define("WEBRTC_POSIX", None)
        .compile("webrtc_wrapper");

    // Link libwebrtc.a static library
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=webrtc");

    // System frameworks required by WebRTC
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=CoreFoundation");
    println!("cargo:rustc-link-lib=framework=CoreMedia");
    println!("cargo:rustc-link-lib=framework=CoreVideo");
    println!("cargo:rustc-link-lib=framework=CoreAudio");
    println!("cargo:rustc-link-lib=framework=AudioToolbox");
    println!("cargo:rustc-link-lib=framework=VideoToolbox");
    println!("cargo:rustc-link-lib=framework=AVFoundation");
    println!("cargo:rustc-link-lib=framework=CoreGraphics");
    println!("cargo:rustc-link-lib=framework=CoreServices");
    println!("cargo:rustc-link-lib=framework=IOSurface");
    println!("cargo:rustc-link-lib=framework=Metal");
    println!("cargo:rustc-link-lib=framework=Security");
    println!("cargo:rustc-link-lib=framework=SystemConfiguration");
    println!("cargo:rustc-link-lib=framework=Network");
    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rustc-link-lib=framework=OpenGL");
    println!("cargo:rustc-link-lib=framework=ScreenCaptureKit");

    // C++ standard library (runtime)
    println!("cargo:rustc-link-lib=c++");
}
