use super::*;
use tempfile::TempDir;

#[test]
fn test_build_system_allowed_bash_prefixes_cargo() {
    let prefixes = BuildSystem::Cargo.allowed_bash_prefixes();
    assert!(prefixes.contains(&"cargo"));
}

#[test]
fn test_build_system_allowed_bash_prefixes_npm() {
    let prefixes = BuildSystem::Npm.allowed_bash_prefixes();
    assert!(prefixes.contains(&"npm"));
    assert!(prefixes.contains(&"npx"));
    assert!(prefixes.contains(&"node"));
}

#[test]
fn test_build_system_allowed_bash_prefixes_python() {
    let prefixes = BuildSystem::Poetry.allowed_bash_prefixes();
    assert!(prefixes.contains(&"poetry"));
    assert!(prefixes.contains(&"python"));
    assert!(prefixes.contains(&"pytest"));
}

#[test]
fn test_build_system_allowed_bash_prefixes_jvm() {
    let gradle = BuildSystem::Gradle.allowed_bash_prefixes();
    assert!(gradle.contains(&"./gradlew"));
    assert!(gradle.contains(&"gradle"));

    let maven = BuildSystem::Maven.allowed_bash_prefixes();
    assert!(maven.contains(&"mvn"));
    assert!(maven.contains(&"./mvnw"));
}

#[test]
fn test_build_system_allowed_bash_prefixes_custom() {
    let cmake = BuildSystem::Custom("cmake".into()).allowed_bash_prefixes();
    assert!(cmake.contains(&"cmake"));
    assert!(cmake.contains(&"make"));
    assert!(cmake.contains(&"ninja"));

    let mix = BuildSystem::Custom("mix".into()).allowed_bash_prefixes();
    assert!(mix.contains(&"mix"));
    assert!(mix.contains(&"elixir"));
}

#[test]
fn test_default_common_prefixes_includes_major_languages() {
    let prefixes = BuildSystem::default_common_prefixes();

    assert!(prefixes.contains(&"cargo"));
    assert!(prefixes.contains(&"npm"));
    assert!(prefixes.contains(&"yarn"));
    assert!(prefixes.contains(&"python"));
    assert!(prefixes.contains(&"pytest"));
    assert!(prefixes.contains(&"go"));
    assert!(prefixes.contains(&"mvn"));
    assert!(prefixes.contains(&"gradle"));
    assert!(prefixes.contains(&"bundle"));
    assert!(prefixes.contains(&"rake"));
    assert!(prefixes.contains(&"composer"));
    assert!(prefixes.contains(&"dotnet"));
    assert!(prefixes.contains(&"swift"));
}

#[test]
fn test_verification_config_resolve_with_cargo_project() {
    let temp = TempDir::new().unwrap();
    std::fs::write(temp.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();

    let config = VerificationConfig::default();
    let prefixes = config.resolve_allowed_bash_prefixes(temp.path());

    assert!(prefixes.contains(&"cargo".to_string()));
}

#[test]
fn test_verification_config_resolve_with_additional() {
    let temp = TempDir::new().unwrap();
    std::fs::write(temp.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();

    let config = VerificationConfig {
        additional_allowed_commands: vec!["docker".into(), "kubectl".into()],
        allowed_commands_exclusive: false,
        ..Default::default()
    };
    let prefixes = config.resolve_allowed_bash_prefixes(temp.path());

    assert!(prefixes.contains(&"cargo".to_string()));
    assert!(prefixes.contains(&"docker".to_string()));
    assert!(prefixes.contains(&"kubectl".to_string()));
}

#[test]
fn test_verification_config_resolve_exclusive() {
    let temp = TempDir::new().unwrap();
    std::fs::write(temp.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();

    let config = VerificationConfig {
        additional_allowed_commands: vec!["custom-build".into()],
        allowed_commands_exclusive: true,
        ..Default::default()
    };
    let prefixes = config.resolve_allowed_bash_prefixes(temp.path());

    assert_eq!(prefixes, vec!["custom-build".to_string()]);
    assert!(!prefixes.contains(&"cargo".to_string()));
}

#[test]
fn test_verification_config_resolve_no_build_system() {
    let temp = TempDir::new().unwrap();

    let config = VerificationConfig::default();
    let prefixes = config.resolve_allowed_bash_prefixes(temp.path());

    assert!(prefixes.contains(&"cargo".to_string()));
    assert!(prefixes.contains(&"npm".to_string()));
    assert!(prefixes.contains(&"python".to_string()));
}
