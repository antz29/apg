use std::path::{Path, PathBuf};
use std::process::Command;

/// Which frontends to compile in this build. `APG_BUILD_FRONTENDS` is a
/// comma-separated allowlist (`go`, `java`, `cpp`); `0`/`none`/empty skips all.
/// Unset = build everything (the dev default). The brew `scanner` formula sets
/// `0`; the per-language `apg-go`/`apg-java`/`apg-cpp` formulae build each
/// frontend directly and don't use build.rs for that.
fn build_frontends() -> Vec<String> {
    match std::env::var("APG_BUILD_FRONTENDS") {
        Ok(v) if v.is_empty() => vec![],
        Ok(v) if v == "0" || v == "none" || v == "false" => vec![],
        Ok(v) => v.split(',').map(|s| s.trim().to_string()).collect(),
        Err(_) => vec!["go".into(), "java".into(), "cpp".into(), "rust".into()],
    }
}

fn enabled(langs: &[String], lang: &str) -> bool {
    langs.iter().any(|l| l == lang)
}

fn main() {
    println!("cargo:rerun-if-changed=src/javalib/CallGraphBuilder.java");
    println!("cargo:rerun-if-changed=src/golib/main.go");
    println!("cargo:rerun-if-changed=src/cpplib/main.cpp");
    println!("cargo:rerun-if-changed=src/rustlib/Cargo.toml");
    println!("cargo:rerun-if-changed=src/rustlib/src/main.rs");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let frontends = build_frontends();
    let mut languages: Vec<String> = Vec::new();

    // Directory the runtime uses to find frontends relative to the binary:
    // <target>/<profile>/frontends. Also populated by `apg`'s runtime lookup
    // (see `frontend_dir` in main.rs) so dev builds and the brew formula can
    // both resolve frontends from a known spot.
    let profile_dir = PathBuf::from(&out_dir)
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .expect("OUT_DIR has unexpected shape")
        .to_path_buf();
    let stage_dir = profile_dir.join("frontends");
    std::fs::create_dir_all(&stage_dir).ok();

    // --- C++ frontend (tree-sitter, no external deps) ---
    if enabled(&frontends, "cpp") {
        let cppfrontend = Path::new(&out_dir).join("cppfrontend");
        let cpplib = Path::new("src/cpplib");
        let vendor = cpplib.join("vendor");

        let runtime_o = Path::new(&out_dir).join("ts_runtime.o");
        let parser_o = Path::new(&out_dir).join("ts_cpp_parser.o");
        let scanner_o = Path::new(&out_dir).join("ts_cpp_scanner.o");
        let main_o = Path::new(&out_dir).join("cppfrontend_main.o");
        let ts_inc = vendor.join("tree-sitter/lib/include");
        let ts_src = vendor.join("tree-sitter/lib/src");
        let cpp_inc = vendor.join("tree-sitter-cpp/src");

        let rt_ok = Command::new("gcc")
            .args([
                "-c", "-fPIC", "-std=c11", "-D_GNU_SOURCE",
                "-I", ts_inc.to_str().unwrap(),
                "-I", ts_src.to_str().unwrap(),
                ts_src.join("lib.c").to_str().unwrap(),
                "-o", runtime_o.to_str().unwrap(),
            ])
            .status()
            .is_ok_and(|s| s.success());

        let parser_ok = Command::new("gcc")
            .args([
                "-c", "-fPIC", "-std=c11",
                "-I", cpp_inc.to_str().unwrap(),
                "-I", ts_inc.to_str().unwrap(),
                vendor.join("tree-sitter-cpp/src/parser.c").to_str().unwrap(),
                "-o", parser_o.to_str().unwrap(),
            ])
            .status()
            .is_ok_and(|s| s.success());

        let scanner_ok = Command::new("gcc")
            .args([
                "-c", "-fPIC", "-std=c11",
                "-I", cpp_inc.to_str().unwrap(),
                "-I", ts_inc.to_str().unwrap(),
                vendor.join("tree-sitter-cpp/src/scanner.c").to_str().unwrap(),
                "-o", scanner_o.to_str().unwrap(),
            ])
            .status()
            .is_ok_and(|s| s.success());

        let main_ok = Command::new("g++")
            .args([
                "-c", "-fPIC", "-std=c++17",
                "-I", ts_inc.to_str().unwrap(),
                "-I", cpp_inc.to_str().unwrap(),
                cpplib.join("main.cpp").to_str().unwrap(),
                "-o", main_o.to_str().unwrap(),
            ])
            .status()
            .is_ok_and(|s| s.success());

        if rt_ok && parser_ok && scanner_ok && main_ok {
            let link_ok = Command::new("g++")
                .args([
                    runtime_o.to_str().unwrap(),
                    parser_o.to_str().unwrap(),
                    scanner_o.to_str().unwrap(),
                    main_o.to_str().unwrap(),
                    "-lm",
                    "-o", cppfrontend.to_str().unwrap(),
                ])
                .status()
                .is_ok_and(|s| s.success());

            if link_ok {
                println!("cargo:rustc-env=APG_FRONTEND_CPP={}", cppfrontend.display());
                let _ = std::fs::copy(&cppfrontend, stage_dir.join("cppfrontend"));
                languages.push("cpp".into());
            }
        }
    }

    // --- Go frontend ---
    if enabled(&frontends, "go") {
        let gofrontend = Path::new(&out_dir).join("gofrontend");
        let go_ok = Command::new("go")
            .args(["build", "-o", gofrontend.to_str().unwrap(), "."])
            .current_dir("src/golib")
            .status()
            .is_ok_and(|s| s.success());

        if go_ok {
            println!("cargo:rustc-env=APG_FRONTEND_GO={}", gofrontend.display());
            let _ = std::fs::copy(&gofrontend, stage_dir.join("gofrontend"));
            languages.push("go".into());
        }
    }

    // --- Rust frontend (rust-analyzer engine, compiled in isolation) ---
    if enabled(&frontends, "rust") {
        // Compile rustlib with cargo into its own isolated target dir
        // (src/rustlib/target), matching the outer build profile (debug for
        // fast dev compile, release for fast scans). Deps resolve from
        // src/rustlib/Cargo.lock; bump the pinned rust-analyzer rev in
        // src/rustlib/Cargo.toml atomically. On failure, skip rust rather
        // than aborting the whole build (like the other frontends).
        let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
        let rustfrontend = Path::new("src/rustlib").join("target").join(&profile).join("rustfrontend");
        let mut cmd = Command::new("cargo");
        cmd.arg("build").arg("--manifest-path").arg("src/rustlib/Cargo.toml");
        if profile == "release" {
            cmd.arg("--release");
        }
        cmd.arg("--bin").arg("rustfrontend");
        let rust_ok = cmd.status().is_ok_and(|s| s.success()) && rustfrontend.exists();
        if rust_ok {
            println!("cargo:rustc-env=APG_FRONTEND_RUST={}", rustfrontend.display());
            let _ = std::fs::copy(&rustfrontend, stage_dir.join("rustfrontend"));
            languages.push("rust".into());
        }
    }

    // --- Java frontend ---
    if enabled(&frontends, "java") {
        let java_classes = Path::new(&out_dir).join("java-classes");
        std::fs::create_dir_all(&java_classes).ok();

        let java_ok = Command::new("javac")
            .args([
                "-d", java_classes.to_str().unwrap(),
                "-proc:none",
                "--add-exports", "jdk.compiler/com.sun.tools.javac.tree=ALL-UNNAMED",
                "--add-exports", "jdk.compiler/com.sun.tools.javac.api=ALL-UNNAMED",
                "--add-exports", "jdk.compiler/com.sun.tools.javac.code=ALL-UNNAMED",
                "--add-exports", "jdk.compiler/com.sun.tools.javac.util=ALL-UNNAMED",
                "src/javalib/CallGraphBuilder.java",
            ])
            .status()
            .is_ok_and(|s| s.success());

        if java_ok {
            let cmd = format!(
                "java -Xmx5g -cp {} --add-exports jdk.compiler/com.sun.source.tree=ALL-UNNAMED --add-exports jdk.compiler/com.sun.source.util=ALL-UNNAMED --add-exports jdk.compiler/com.sun.tools.javac.tree=ALL-UNNAMED --add-exports jdk.compiler/com.sun.tools.javac.api=ALL-UNNAMED --add-exports jdk.compiler/com.sun.tools.javac.code=ALL-UNNAMED --add-exports jdk.compiler/com.sun.tools.javac.util=ALL-UNNAMED CallGraphBuilder",
                java_classes.display()
            );
            println!("cargo:rustc-env=APG_FRONTEND_JAVA={}", cmd);
            let stage_java = stage_dir.join("java-classes");
            std::fs::create_dir_all(&stage_java).ok();
            copy_dir(&java_classes, &stage_java);
            languages.push("java".into());
        }
    }

    println!("cargo:rustc-env=APG_LANGUAGES={}", languages.join(","));
}

fn copy_dir(from: &Path, to: &Path) {
    let entries = match std::fs::read_dir(from) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            std::fs::create_dir_all(&dst).ok();
            copy_dir(&src, &dst);
        } else {
            let _ = std::fs::copy(&src, &dst);
        }
    }
}
