use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TowerConfig {
    #[serde(default)]
    pub repo: RepoCfg,
    #[serde(default)]
    pub gates: GatesCfg,
    #[serde(default)]
    pub reserved: ReservedCfg,
    #[serde(default)]
    pub escalation: EscalationCfg,
    #[serde(default)]
    pub limits: LimitsCfg,
    #[serde(default)]
    pub feedback: FeedbackCfg,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoCfg {
    #[serde(default = "default_main")]
    pub main_branch: String,
}
fn default_main() -> String { "main".into() }
impl Default for RepoCfg { fn default() -> Self { Self { main_branch: default_main() } } }
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GatesCfg { pub check: Option<String>, pub test: Option<String> }
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReservedCfg { #[serde(default)] pub paths: Vec<String>, #[serde(default)] pub patterns: Vec<String> }
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EscalationCfg { #[serde(default)] pub extra_triggers: Vec<String> }
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LimitsCfg { pub max_concurrent_assignments: Option<u32> }

/// Self-reporting target (Phase 2.3): Tower bugs/inefficiencies are filed
/// as GitHub issues on this repo. `enabled = false` records locally only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackCfg {
    #[serde(default = "default_feedback_repo")]
    pub repo: String,
    #[serde(default = "default_feedback_enabled")]
    pub enabled: bool,
}
fn default_feedback_repo() -> String { "Holy-Coders/tower".into() }
fn default_feedback_enabled() -> bool { true }
impl Default for FeedbackCfg { fn default() -> Self { Self { repo: default_feedback_repo(), enabled: true } } }

impl TowerConfig {
    pub fn load(repo_root: &Path) -> Self {
        let p = repo_root.join("tower.toml");
        match std::fs::read_to_string(&p) {
            Ok(s) => toml::from_str(&s).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }
    pub fn tier_label(&self) -> &'static str {
        match (self.gates.check.is_some(), self.gates.test.is_some()) {
            (true, true) => "Tier-0 simulation + Tier-1 check + Tier-2 test gates configured",
            (true, false) => "Tier-0 simulation + Tier-1 check gate configured (no Tier-2 test gate)",
            (false, true) => "Tier-0 simulation + Tier-2 test gate configured (no Tier-1 check gate)",
            (false, false) => "Tier-0 simulation only (no gates configured)",
        }
    }
    pub fn has_gates(&self) -> bool { self.gates.check.is_some() || self.gates.test.is_some() }
}

pub fn default_config_text(main_branch: &str, repo_root: &Path) -> String {
    let (check, test) = infer_gates(repo_root);
    let check_line = check.map(|c| format!("check = \"{c}\"")).unwrap_or_else(|| "# check = \"cargo check\"".into());
    let test_line = test.map(|c| format!("test = \"{c}\"")).unwrap_or_else(|| "# test = \"cargo test\"".into());
    format!("[repo]\nmain_branch = \"{main_branch}\"\n\n[gates]\n{check_line}\n{test_line}\n\n[reserved]\npaths = [\"**/migrations/**\", \"build.rs\", \"**/*.lock\"]\npatterns = [\"macro_rules!\", \"proc_macro\"]\n\n[escalation]\nextra_triggers = []\n\n[limits]\nmax_concurrent_assignments = 4\n\n[feedback]\nrepo = \"Holy-Coders/tower\"\nenabled = true\n")
}

fn infer_gates(repo_root: &Path) -> (Option<&'static str>, Option<&'static str>) {
    if repo_root.join("Cargo.toml").exists() { return (Some("cargo check"), Some("cargo test")); }
    if repo_root.join("package.json").exists() { return (Some("npm run build --if-present"), Some("npm test")); }
    if repo_root.join("go.mod").exists() { return (Some("go vet ./..."), Some("go test ./...")); }
    (None, None)
}

pub fn tower_home() -> PathBuf {
    if let Ok(v) = std::env::var("TOWER_HOME") { if !v.is_empty() { return PathBuf::from(v); } }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".tower")
}

pub fn repo_hash(repo_canonical: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in repo_canonical.as_bytes() { h ^= *b as u64; h = h.wrapping_mul(0x100000001b3); }
    format!("{h:016x}")
}
