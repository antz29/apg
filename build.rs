use std::path::Path;
use std::process::Command;

fn main() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let java_classes = Path::new(&out_dir).join("java-classes");

    println!("cargo:rerun-if-changed=src/javalib/CallGraphBuilder.java");

    std::fs::create_dir_all(&java_classes).unwrap();
    let status = Command::new("javac")
        .args([
            "-d",
            java_classes.to_str().unwrap(),
            "-proc:none",
            "--add-exports",
            "jdk.compiler/com.sun.tools.javac.tree=ALL-UNNAMED",
            "src/javalib/CallGraphBuilder.java",
        ])
        .status()
        .expect("Failed to compile Java helper. Install a JDK (`javac` required).");

    assert!(status.success(), "Java compilation failed");

    let cmd = format!(
        "java -cp {} --add-exports jdk.compiler/com.sun.source.tree=ALL-UNNAMED --add-exports jdk.compiler/com.sun.source.util=ALL-UNNAMED --add-exports jdk.compiler/com.sun.tools.javac.tree=ALL-UNNAMED CallGraphBuilder",
        java_classes.display()
    );
    println!("cargo:rustc-env=APG_FRONTEND_CMD={}", cmd);
}
