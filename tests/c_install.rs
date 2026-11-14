//! `make install` as a C user runs it: the installed files, the soname, and
//! the example built through pkg-config and CMake, linked both shared and
//! static, then `make uninstall`. Linux only; skipped without make, a C
//! compiler or pkg-config, and the CMake part without cmake.
mod common;

use common::*;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn have(tool: &str) -> bool {
    Command::new(tool).arg("--version").output().is_ok_and(|o| o.status.success())
}

fn run(cmd: &mut Command) -> Output {
    let out = cmd.output().unwrap_or_else(|e| panic!("{cmd:?}: {e}"));
    assert!(
        out.status.success(),
        "{cmd:?} failed\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn text(out: Output) -> String {
    String::from_utf8(out.stdout).unwrap()
}

fn make(target: &str, prefix: &Path) -> Output {
    run(Command::new("make")
        .arg(target)
        .arg(format!("PREFIX={}", prefix.display()))
        .args(["PROFILE=dev", "TARGET_DIR=target/c-install/build", "CARGO_FLAGS=--offline"])
        .current_dir(root()))
}

/// Two KLV PES packets in a small program.
fn sample(dir: &Path) -> PathBuf {
    let mut ts = Ts::new();
    ts.section(0, &pat(5, 0, &[(1, 0x1000)]));
    ts.section(0x1000, &pmt(1, 0, 0x100, &[], &[(H264, 0x100, vec![]), (PRIVATE, 0x102, registration(b"KLVA"))]));
    for i in 0..2u64 {
        ts.pes(0x100, 0xE0, Some(i * 3003), &[0; 400]);
        ts.pes(0x102, 0xBD, Some(500 + i), &KLV_KEY);
    }
    let path = dir.join("sample.ts");
    std::fs::write(&path, &ts.out).unwrap();
    path
}

const EXPECTED: &str = "pid 0x0102  16 bytes  pts 500 (0.006 s)\npid 0x0102  16 bytes  pts 501 (0.006 s)\n";

fn needed_libraries(exe: &Path) -> Option<String> {
    let out = Command::new("readelf").arg("-d").arg(exe).output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn install_then_build_with_pkg_config_and_cmake() {
    if !cfg!(target_os = "linux") {
        eprintln!("skipping: the layout checked here is Linux's");
        return;
    }
    for tool in ["make", "cc", "pkg-config"] {
        if !have(tool) {
            eprintln!("skipping: no {tool}");
            return;
        }
    }
    let work = root().join(format!("target/c-install/run-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let prefix = work.join("prefix");
    let lib = prefix.join("lib");
    make("install", &prefix);

    // Layout.
    for file in [
        "lib/libmpegts.so.1",
        "lib/libmpegts.a",
        "include/mpegts.h",
        "lib/pkgconfig/mpegts.pc",
        "lib/cmake/mpegts/mpegts-config.cmake",
        "lib/cmake/mpegts/mpegts-config-version.cmake",
    ] {
        assert!(prefix.join(file).is_file(), "{file} not installed");
    }
    assert_eq!(std::fs::read_link(lib.join("libmpegts.so")).unwrap(), Path::new("libmpegts.so.1"));
    assert_eq!(
        std::fs::read(prefix.join("include/mpegts.h")).unwrap(),
        std::fs::read(root().join("include/mpegts.h")).unwrap()
    );
    if let Some(dynamic) = needed_libraries(&lib.join("libmpegts.so.1")) {
        assert!(dynamic.contains("Library soname: [libmpegts.so.1]"), "{dynamic}");
    }

    let input = sample(&work);
    let pkg_config = |args: &[&str]| -> Vec<String> {
        let out = run(Command::new("pkg-config").args(args).arg("mpegts").env("PKG_CONFIG_PATH", lib.join("pkgconfig")));
        text(out).split_whitespace().map(String::from).collect()
    };
    assert_eq!(pkg_config(&["--modversion"]), [env!("CARGO_PKG_VERSION")]);
    let example = root().join("examples/c/klv_dump.c");

    // Shared, through pkg-config.
    let shared = work.join("klv_dump_shared");
    run(Command::new("cc")
        .args(["-std=c99", "-Wall", "-Wextra", "-Werror"])
        .arg(&example)
        .args(pkg_config(&["--cflags", "--libs"]))
        .arg("-o")
        .arg(&shared));
    let out = text(run(Command::new(&shared).arg(&input).env("LD_LIBRARY_PATH", &lib)));
    assert_eq!(out, EXPECTED);
    if let Some(dynamic) = needed_libraries(&shared) {
        assert!(dynamic.contains("[libmpegts.so.1]"), "linked by soname: {dynamic}");
    }

    // Static, with the system libraries pkg-config lists for it.
    let fixed = work.join("klv_dump_static");
    let system_libs: Vec<String> =
        pkg_config(&["--static", "--libs-only-l"]).into_iter().filter(|l| l != "-lmpegts").collect();
    assert!(!system_libs.is_empty(), "Libs.private is empty");
    run(Command::new("cc")
        .args(["-std=c99", "-Wall", "-Wextra", "-Werror"])
        .args(pkg_config(&["--cflags"]))
        .arg(&example)
        .arg(lib.join("libmpegts.a"))
        .args(&system_libs)
        .arg("-o")
        .arg(&fixed));
    assert_eq!(text(run(Command::new(&fixed).arg(&input))), EXPECTED);
    if let Some(dynamic) = needed_libraries(&fixed) {
        assert!(!dynamic.contains("libmpegts"), "static build still needs libmpegts: {dynamic}");
    }

    // CMake: find_package, both targets, and the version check.
    if have("cmake") {
        for (static_link, name) in [("OFF", "cmake-shared"), ("ON", "cmake-static")] {
            let build = work.join(name);
            run(Command::new("cmake")
                .arg("-S")
                .arg(root().join("examples/c"))
                .arg("-B")
                .arg(&build)
                .arg(format!("-DCMAKE_PREFIX_PATH={}", prefix.display()))
                .arg(format!("-DMPEGTS_STATIC={static_link}")));
            run(Command::new("cmake").arg("--build").arg(&build));
            assert_eq!(text(run(Command::new(build.join("klv_dump")).arg(&input))), EXPECTED, "{name}");
        }
        let project = work.join("needs-newer");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.10)\nproject(t C)\nfind_package(mpegts 0.2 REQUIRED)\n",
        )
        .unwrap();
        let refused = Command::new("cmake")
            .arg("-S")
            .arg(&project)
            .arg("-B")
            .arg(project.join("build"))
            .arg(format!("-DCMAKE_PREFIX_PATH={}", prefix.display()))
            .output()
            .unwrap();
        let why = String::from_utf8_lossy(&refused.stderr);
        assert!(!refused.status.success(), "0.1 must not satisfy a request for 0.2");
        assert!(why.contains("considered but not accepted") && why.contains("version: 0.1.0"), "{why}");
    } else {
        eprintln!("skipping the CMake part: no cmake");
    }

    // Uninstall leaves no files behind.
    make("uninstall", &prefix);
    let mut left = Vec::new();
    let mut stack = vec![prefix.clone()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                left.push(path);
            }
        }
    }
    assert!(left.is_empty(), "left after uninstall: {left:?}");
    let _ = std::fs::remove_dir_all(&work);
}
