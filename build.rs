// Builds wl-kbptr's target detection (csrc/) when the `detect` feature is on,
// against the system OpenCV (5, falling back to 4) and pixman.
fn main() {
    println!("cargo:rerun-if-changed=csrc");
    if std::env::var_os("CARGO_FEATURE_DETECT").is_none() {
        return;
    }

    let opencv = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("opencv5")
        .or_else(|_| pkg_config::Config::new().cargo_metadata(false).probe("opencv4"))
        .expect("`detect` feature needs OpenCV 5 or 4 (pkg-config opencv5/opencv4)");
    let pixman = pkg_config::probe_library("pixman-1").expect("`detect` feature needs pixman");
    let wayland = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("wayland-client")
        .expect("`detect` feature needs wayland-client headers");

    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .file("csrc/target_detection.cpp")
        .define("OPENCV_ENABLED", "1")
        .warnings(false);
    for p in opencv.include_paths.iter().chain(&pixman.include_paths).chain(&wayland.include_paths) {
        build.include(p);
    }
    build.compile("keeby_detect");

    // Only the modules the detection uses; `geometry` exists from OpenCV 5.
    for path in &opencv.link_paths {
        println!("cargo:rustc-link-search=native={}", path.display());
    }
    for lib in ["opencv_core", "opencv_imgproc", "opencv_geometry"] {
        if opencv.libs.iter().any(|l| l == lib) {
            println!("cargo:rustc-link-lib=dylib={lib}");
        }
    }
    println!("cargo:rustc-link-lib=dylib=stdc++");
}
