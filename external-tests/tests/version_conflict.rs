use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const REACS_MANIFEST: &str = include_str!("../../reacs/Cargo.toml");

fn write(path: PathBuf, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn write_reacs_stub(dir: &Path, version: &str) {
    let package = REACS_MANIFEST.split("[dependencies]").next().unwrap();
    let manifest: Vec<String> = package
        .lines()
        .map(|line| match line.starts_with("version") {
            true => format!("version = \"{version}\""),
            false => line.to_string(),
        })
        .collect();

    write(dir.join("Cargo.toml"), &manifest.join("\n"));
    write(dir.join("build.rs"), "fn main() {}");
    write(dir.join("src/lib.rs"), "");
}

fn resolve_app(case: &str, plugin_reacs_version: &str) -> Output {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(case);
    let _ = fs::remove_dir_all(&root);

    write_reacs_stub(&root.join("reacs_app"), "1.0.0");
    let plugin_reacs = match plugin_reacs_version {
        "1.0.0" => "../reacs_app",
        version => {
            write_reacs_stub(&root.join("reacs_plugin"), version);
            "../reacs_plugin"
        }
    };

    write(
        root.join("plugin/Cargo.toml"),
        &format!(
            "[package]\nname = \"plugin\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n\
             [dependencies]\nreacs = {{ path = \"{plugin_reacs}\" }}\n"
        ),
    );
    write(root.join("plugin/src/lib.rs"), "");
    write(
        root.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n\
         [dependencies]\nreacs = { path = \"reacs_app\" }\nplugin = { path = \"plugin\" }\n\n\
         [workspace]\nexclude = [\"reacs_app\", \"reacs_plugin\", \"plugin\"]\n",
    );
    write(root.join("src/main.rs"), "fn main() {}");

    Command::new(std::env::var("CARGO").unwrap())
        .args(["metadata", "--offline", "--format-version", "1"])
        .current_dir(&root)
        .output()
        .unwrap()
}

#[test]
#[cfg_attr(miri, ignore)]
fn single_reacs_version_resolves() {
    let output = resolve_app("single_version", "1.0.0");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg_attr(miri, ignore)]
fn mismatched_reacs_versions_fail_to_resolve() {
    let output = resolve_app("mismatched_versions", "2.0.0");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(stderr.contains("links"), "{stderr}");
}
