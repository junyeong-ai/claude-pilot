use crate::error::PilotError;
use crate::verification::convergent::analysis::detect_primary_language;
use crate::verification::convergent::types::{ConvergenceResult, PatternContext};
use crate::verification::convergent::ConvergentVerifier;
use crate::verification::history::{ConvergenceState, FixStrategy, VerificationHistory};

fn empty_history() -> VerificationHistory {
    VerificationHistory::new("test-mission")
}

fn empty_state() -> ConvergenceState {
    ConvergenceState::new("test-mission", 2, 10, 3)
}

// -- PatternContext --

#[test]
fn test_pattern_context_format_high_confidence() {
    let ctx = PatternContext {
        pattern_id: "p1".into(),
        strategy: FixStrategy::DirectFix,
        confidence: 0.85,
        success_rate: 0.9,
        total_uses: 10,
    };
    let text = ctx.format_for_llm();
    assert!(text.contains("HIGH"));
    assert!(text.contains("85%"));
}

#[test]
fn test_pattern_context_format_moderate_confidence() {
    let ctx = PatternContext {
        pattern_id: "p2".into(),
        strategy: FixStrategy::ContextualFix,
        confidence: 0.65,
        success_rate: 0.7,
        total_uses: 5,
    };
    let text = ctx.format_for_llm();
    assert!(text.contains("MODERATE"));
}

#[test]
fn test_pattern_context_format_low_confidence() {
    let ctx = PatternContext {
        pattern_id: "p3".into(),
        strategy: FixStrategy::DependencyFix,
        confidence: 0.45,
        success_rate: 0.5,
        total_uses: 2,
    };
    let text = ctx.format_for_llm();
    assert!(text.contains("LOW"));
}

#[test]
fn test_pattern_context_format_experimental_confidence() {
    let ctx = PatternContext {
        pattern_id: "p4".into(),
        strategy: FixStrategy::RefactorFix,
        confidence: 0.2,
        success_rate: 0.3,
        total_uses: 1,
    };
    let text = ctx.format_for_llm();
    assert!(text.contains("EXPERIMENTAL"));
}

// -- ConvergenceResult constructors --

#[test]
fn test_convergence_result_converged() {
    let result = ConvergenceResult::converged(empty_history(), empty_state());
    assert!(result.converged);
    assert!(result.termination_reason.is_none());
    assert!(result.persistent_issues.is_empty());
}

#[test]
fn test_convergence_result_not_converged() {
    let result = ConvergenceResult::not_converged(empty_history(), empty_state(), 0.90);
    assert!(!result.converged);
    assert!(result.termination_reason.is_none());
}

#[test]
fn test_convergence_result_oscillating() {
    let result = ConvergenceResult::oscillating(empty_history(), empty_state(), 0.90);
    assert!(!result.converged);
    assert!(result.termination_reason.is_some());
}

#[test]
fn test_convergence_result_early_terminated() {
    let reason = crate::verification::solvability::EarlyTerminationReason::Oscillation;
    let result = ConvergenceResult::early_terminated(empty_history(), empty_state(), reason, 0.90);
    assert!(!result.converged);
    assert!(result.termination_reason.is_some());
}

// -- extension_to_language --

#[test]
fn test_extension_to_language_rust() {
    assert_eq!(
        ConvergentVerifier::extension_to_language("src/main.rs"),
        Some("Rust")
    );
}

#[test]
fn test_extension_to_language_typescript() {
    assert_eq!(
        ConvergentVerifier::extension_to_language("app.tsx"),
        Some("TypeScript")
    );
}

#[test]
fn test_extension_to_language_python() {
    assert_eq!(
        ConvergentVerifier::extension_to_language("script.py"),
        Some("Python")
    );
}

#[test]
fn test_extension_to_language_go() {
    assert_eq!(
        ConvergentVerifier::extension_to_language("main.go"),
        Some("Go")
    );
}

#[test]
fn test_extension_to_language_java() {
    assert_eq!(
        ConvergentVerifier::extension_to_language("App.java"),
        Some("Java")
    );
}

#[test]
fn test_extension_to_language_unknown() {
    assert_eq!(ConvergentVerifier::extension_to_language("data.csv"), None);
}

// -- get_language_hints --

#[test]
fn test_get_language_hints_rust() {
    let hints = ConvergentVerifier::get_language_hints("lib.rs");
    assert!(hints.is_some());
    let text = hints.unwrap();
    assert!(text.contains("Rust"));
    assert!(text.contains("ownership"));
}

#[test]
fn test_get_language_hints_python() {
    let hints = ConvergentVerifier::get_language_hints("app.py");
    assert!(hints.is_some());
    let text = hints.unwrap();
    assert!(text.contains("Python"));
}

#[test]
fn test_get_language_hints_unknown() {
    assert!(ConvergentVerifier::get_language_hints("data.csv").is_none());
}

// -- detect_project_context --

#[test]
fn test_detect_project_context_with_cargo() {
    let temp = tempfile::TempDir::new().unwrap();
    std::fs::write(temp.path().join("Cargo.toml"), "[package]").unwrap();

    let ctx = ConvergentVerifier::detect_project_context(
        temp.path(),
        &["src/main.rs"],
        Some("src/main.rs"),
    );
    assert!(ctx.contains("Rust") || ctx.contains("Cargo"));
}

#[test]
fn test_detect_project_context_polyglot() {
    let temp = tempfile::TempDir::new().unwrap();
    let ctx = ConvergentVerifier::detect_project_context(
        temp.path(),
        &["src/main.rs", "script.py"],
        None,
    );
    assert!(ctx.contains("Polyglot") || ctx.contains("Rust") || ctx.contains("Python"));
}

// -- detect_primary_language --

#[test]
fn test_detect_primary_language_none_for_no_files() {
    assert!(detect_primary_language(None).is_none());
}

#[test]
fn test_detect_primary_language_empty_files() {
    let created: Vec<String> = vec![];
    let modified: Vec<String> = vec![];
    assert!(detect_primary_language(Some((&created, &modified))).is_none());
}

#[test]
fn test_detect_primary_language_rust_files() {
    let created = vec!["src/main.rs".to_string(), "src/lib.rs".to_string()];
    let modified = vec!["Cargo.toml".to_string()];
    let result = detect_primary_language(Some((&created, &modified)));
    assert!(result.is_some());
    let detection = result.unwrap();
    assert!(!detection.languages.is_empty());
    assert_eq!(detection.languages[0].name, "Rust");
}

#[test]
fn test_detect_primary_language_unknown_extensions() {
    let created = vec!["data.csv".to_string(), "notes.txt".to_string()];
    let modified: Vec<String> = vec![];
    assert!(detect_primary_language(Some((&created, &modified))).is_none());
}

// -- LanguageDetection::format_for_llm --

#[test]
fn test_language_detection_format_empty() {
    use crate::verification::convergent::analysis::LanguageDetection;
    let detection = LanguageDetection { languages: vec![] };
    assert_eq!(detection.format_for_llm(), "");
}

#[test]
fn test_language_detection_format_single() {
    use crate::verification::convergent::analysis::{
        DetectedLanguage, LanguageConfidence, LanguageDetection,
    };
    let detection = LanguageDetection {
        languages: vec![DetectedLanguage {
            name: "Rust",
            confidence: LanguageConfidence::High,
        }],
    };
    let text = detection.format_for_llm();
    assert!(text.contains("Rust"));
    assert!(text.contains("verified"));
}

// -- is_context_overflow_error --

#[test]
fn test_is_context_overflow_error_true() {
    let err =
        PilotError::AgentExecution("prompt is too long: 212893 tokens > 200000 maximum".to_string());
    assert!(ConvergentVerifier::is_context_overflow_error(&err));
}

#[test]
fn test_is_context_overflow_error_false_for_generic() {
    let err = PilotError::AgentExecution("some generic error".to_string());
    assert!(!ConvergentVerifier::is_context_overflow_error(&err));
}

#[test]
fn test_is_context_overflow_error_false_for_non_agent() {
    let err = PilotError::Config("config error".to_string());
    assert!(!ConvergentVerifier::is_context_overflow_error(&err));
}
