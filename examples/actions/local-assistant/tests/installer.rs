use std::fs;
use std::path::Path;
use std::process::Command;

fn install(package: &Path, home: &Path, config: &Path) -> std::process::Output {
    Command::new("bash")
        .arg(package.join("install"))
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", config)
        .output()
        .unwrap()
}

#[test]
fn native_installer_preserves_settings_backs_up_config_and_stays_manual() {
    let sandbox = tempfile::tempdir().unwrap();
    let package = sandbox.path().join("package");
    let home = sandbox.path().join("home");
    let config = sandbox.path().join("config");
    fs::create_dir(&package).unwrap();
    fs::create_dir(&home).unwrap();
    let recorder = config.join("omarchy-meeting-recorder");
    fs::create_dir_all(&recorder).unwrap();
    let original =
        "model = \"small\"\n\n[[action]]\nname = \"Existing action\"\ncommand = \"echo hello\"\n";
    fs::write(recorder.join("config.toml"), original).unwrap();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("install"),
        package.join("install"),
    )
    .unwrap();
    fs::copy(
        env!("CARGO_BIN_EXE_meeting-local-assistant"),
        package.join("meeting-local-assistant"),
    )
    .unwrap();
    let outcome = install(&package, &home, &config);
    assert!(
        outcome.status.success(),
        "{}",
        String::from_utf8_lossy(&outcome.stderr)
    );
    let updated = fs::read_to_string(recorder.join("config.toml")).unwrap();
    assert!(updated.starts_with(original));
    assert!(updated.contains("name = \"Local meeting recap (Ollama)\""));
    assert!(!updated.contains("auto = true"));
    let backups: Vec<_> = fs::read_dir(&recorder)
        .unwrap()
        .map(|x| x.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("config.toml.before-assistant.")
        })
        .collect();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read_to_string(&backups[0]).unwrap(), original);
    let binary = home.join(".local/bin/meeting-local-assistant");
    assert!(Command::new(&binary)
        .arg("--version")
        .output()
        .unwrap()
        .status
        .success());
    assert!(!install(&package, &home, &config).status.success());
    assert_eq!(
        fs::read_to_string(recorder.join("config.toml")).unwrap(),
        updated
    );
}

#[test]
fn installer_refuses_a_symlinked_config_without_changing_it() {
    let sandbox = tempfile::tempdir().unwrap();
    let home = sandbox.path().join("home");
    fs::create_dir(&home).unwrap();
    let config = sandbox.path().join("config");
    let recorder = config.join("omarchy-meeting-recorder");
    fs::create_dir_all(&recorder).unwrap();
    let original = sandbox.path().join("original.toml");
    fs::write(&original, "model = 'small'\n").unwrap();
    std::os::unix::fs::symlink(&original, recorder.join("config.toml")).unwrap();
    let outcome = install(Path::new(env!("CARGO_MANIFEST_DIR")), &home, &config);
    assert!(!outcome.status.success());
    assert!(String::from_utf8_lossy(&outcome.stderr).contains("symlink"));
    assert_eq!(fs::read_to_string(&original).unwrap(), "model = 'small'\n");
    assert!(!home.join(".local/bin/meeting-local-assistant").exists());
}
