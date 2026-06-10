//! Agent catalog — `~/.config/dutabo/config.jsonc` (schema v1).
//!
//! The v1 document is the ONLY accepted shape (JSONC: comments + trailing
//! commas allowed; unknown keys REJECTED via `deny_unknown_fields`):
//!
//! ```jsonc
//! { "version": 1,
//!   "agents": { "claude-code": {
//!       "deploy": { "dst": ".claude", "mode": "sync", "post_hook": "install.sh" },
//!       "targets": {
//!         "Rockchip": { "items": {
//!             "AOSP": { "src": "https://git.example.com/agent/rk-aosp.git", "ref": "main" }
//!         } },
//!         "Rust": { "src": "https://git.example.com/agent/rust.git" }
//!       }
//! } } }
//! ```
//!
//! Path model (spec §2): exactly two deployable shapes —
//!
//! * Agent → Leaf            (2 levels; e.g. `claude-code / Rust`)
//! * Agent → Group → Leaf    (3 levels; e.g. `claude-code / Rockchip / AOSP`)
//!
//! A target is either a `Leaf` (`src`, optional `ref`/`post_hook`) or a
//! `Group` (`items` of Leaves). Nothing nests deeper; all names are opaque
//! user keys — no vendor/os semantics exist here.
//!
//! Effective config (spec §8): `src`/`ref` come from the leaf, `dst`/`mode`
//! from the agent `deploy` block, `post_hook` = `leaf.post_hook ??
//! agent.deploy.post_hook`.
//!
//! Source order is preserved everywhere (serde_json `preserve_order`) —
//! agents/targets/items render in insertion order, never sorted.
//!
//! Path resolution: `DUTABO_AGENT_CATALOG` env (tests/CI) →
//! `$XDG_CONFIG_HOME/dutabo/config.jsonc` →
//! `$HOME/.config/dutabo/config.jsonc`. A MISSING file is
//! `CatalogStatus::Missing` (a hint, not an error); parse/validation
//! failures are `CatalogStatus::Invalid(path, reason)`.

use std::path::{Component, Path, PathBuf};

/// The catalog file name under the dutabo config dir.
pub const CONFIG_FILE_NAME: &str = "config.jsonc";
/// Env override for the catalog path (mirrors the TARGET_CONF precedent).
pub const AGENT_CATALOG_ENV: &str = "DUTABO_AGENT_CATALOG";
/// The only accepted document version.
pub const CURRENT_VERSION: u32 = 1;

/// Schema limits (`$defs/name`, `leaf.src`, `leaf.ref`, `deploy.dst`,
/// `deploy.post_hook`) — enforced here because hand-rolled validation
/// replaces the JSON Schema validator (no jsonschema dependency).
const NAME_MAX: usize = 64;
const SRC_MAX: usize = 2048;
const REF_MAX: usize = 256;
const DST_MAX: usize = 256;
const HOOK_MAX: usize = 256;

/// An insertion-ordered object map: `Vec<(String, T)>` pairs deserialized
/// from a JSON object with SOURCE ORDER preserved. `serde_json::Map` only
/// exposes its API for `Map<String, Value>`, and serde has no built-in
/// `Vec<(K, V)>`-from-map support — the `preserve_order` Map iterates in
/// source order, so `visit_map` captures it here and `Deref` keeps the
/// full Vec API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedMap<T>(pub Vec<(String, T)>);

impl<T> std::ops::Deref for OrderedMap<T> {
    type Target = Vec<(String, T)>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> Default for OrderedMap<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<'a, T> IntoIterator for &'a OrderedMap<T> {
    type Item = &'a (String, T);
    type IntoIter = std::slice::Iter<'a, (String, T)>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for OrderedMap<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{MapAccess, Visitor};
        use std::marker::PhantomData;
        struct Ordered<T>(PhantomData<fn() -> T>);
        impl<'de, T: serde::Deserialize<'de>> Visitor<'de> for Ordered<T> {
            type Value = Vec<(String, T)>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("an object of string keys")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::with_capacity(map.size_hint().unwrap_or(0));
                while let Some((k, v)) = map.next_entry()? {
                    out.push((k, v));
                }
                Ok(out)
            }
        }
        deserializer
            .deserialize_map(Ordered(PhantomData))
            .map(OrderedMap)
    }
}

/// Root catalog document: `version` + ordered `agents` map.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentCatalog {
    pub version: u32,
    pub agents: OrderedMap<AgentEntry>,
}

/// One agent: the `deploy` block + the ordered `targets` map.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentEntry {
    pub deploy: DeployConfig,
    pub targets: OrderedMap<Target>,
}

/// The agent-level deploy block. `post_hook` is OPTIONAL here (spec v1);
/// the effective hook is `leaf.post_hook ?? agent.deploy.post_hook`.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployConfig {
    pub dst: String,
    /// `"sync"` or `"merge"` — validated semantically (spec §9.2).
    pub mode: String,
    #[serde(default)]
    pub post_hook: Option<String>,
}

/// One target of an agent: a deployable Leaf or a Group of Leaves (schema
/// `oneOf`; the discriminator is `items` vs `src`). Source order preserved.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Leaf(Leaf),
    Group(Group),
}

/// A group of deployable leaves (spec: items are Leaves ONLY — no nesting).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub items: OrderedMap<Leaf>,
}

/// One deployable agent repo: `src` required; `ref` and `post_hook`
/// optional (the hook overrides the agent-level one).
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Leaf {
    pub src: String,
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub post_hook: Option<String>,
}

/// Manual `Target` deserialization: discriminate Leaf vs Group by the
/// presence of `src` vs `items` (schema `oneOf`). Delegating to the
/// variant's own `Deserialize` keeps the `deny_unknown_fields` error
/// quality — an unknown leaf key like `remot` is NAMED, never swallowed
/// by a generic untagged-enum message.
impl<'de> serde::Deserialize<'de> for Target {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let has_items = value.get("items").is_some();
        let has_src = value.get("src").is_some();
        match (has_items, has_src) {
            (true, false) => serde_json::from_value::<Group>(value)
                .map(Target::Group)
                .map_err(|e| serde::de::Error::custom(e.to_string())),
            (false, true) => serde_json::from_value::<Leaf>(value)
                .map(Target::Leaf)
                .map_err(|e| serde::de::Error::custom(e.to_string())),
            _ => Err(serde::de::Error::custom(
                "target must be a leaf ({\"src\": …}) or a group ({\"items\": …})",
            )),
        }
    }
}

impl Target {
    pub fn as_leaf(&self) -> Option<&Leaf> {
        match self {
            Target::Leaf(l) => Some(l),
            Target::Group(_) => None,
        }
    }

    pub fn as_group(&self) -> Option<&Group> {
        match self {
            Target::Leaf(_) => None,
            Target::Group(g) => Some(g),
        }
    }
}

impl Group {
    /// The item (leaf) keys in source order.
    pub fn items(&self) -> Vec<&str> {
        self.items.iter().map(|(k, _)| k.as_str()).collect()
    }
}

/// The effective deployment config of ONE deployable path (spec §8):
/// `src`/`ref` from the leaf; `dst`/`mode` from the agent deploy block;
/// `post_hook` = `leaf.post_hook ?? agent.deploy.post_hook`.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveConfig {
    pub agent: String,
    /// 2-level: the leaf name; 3-level: the GROUP name.
    pub target: String,
    /// 3-level: the leaf name inside the group; None for a 2-level path.
    pub item: Option<String>,
    pub src: String,
    pub r#ref: Option<String>,
    pub dst: String,
    pub mode: String,
    pub post_hook: Option<String>,
}

impl EffectiveConfig {
    /// The path rendered as `agent/target[/item]` (TUI breadcrumb, CLI).
    pub fn path(&self) -> String {
        match &self.item {
            Some(item) => format!("{}/{}/{}", self.agent, self.target, item),
            None => format!("{}/{}", self.agent, self.target),
        }
    }
}

/// Catalog load state surfaced to the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum CatalogStatus {
    /// Parsed and validated.
    Loaded(PathBuf),
    /// No file — the TUI renders the path + a hint; deploy is disabled.
    Missing(PathBuf),
    /// File exists but does not parse/validate (the reason is shown).
    Invalid(PathBuf, String),
}

impl AgentCatalog {
    /// Empty catalog (no agents) — the Missing/Invalid fallback.
    pub fn empty() -> Self {
        Self {
            version: CURRENT_VERSION,
            agents: OrderedMap::default(),
        }
    }

    /// Parse + validate JSONC text. Errors follow the house convention:
    /// strict parse errors and `deny_unknown_fields` failures name the
    /// offending key; validation failures name the agent/target/item path.
    pub fn parse(text: &str) -> Result<AgentCatalog, String> {
        // The exact house JSONC convention (parse_jsonc_value, config.rs):
        // comments + trailing commas allowed, everything else strict.
        let value = crate::config::parse_jsonc_value(text)?;
        let catalog: AgentCatalog =
            serde_json::from_value(value).map_err(|e| format!("catalog schema error: {e}"))?;
        validate(&catalog)?;
        Ok(catalog)
    }

    /// The agent keys in source order.
    pub fn agents(&self) -> Vec<&str> {
        self.agents.iter().map(|(k, _)| k.as_str()).collect()
    }

    /// One agent's entry (None for an unknown agent).
    pub fn entry(&self, agent: &str) -> Option<&AgentEntry> {
        self.agents.iter().find(|(k, _)| k == agent).map(|(_, e)| e)
    }

    /// One agent's target keys in source order (None for an unknown agent).
    pub fn targets(&self, agent: &str) -> Option<Vec<&str>> {
        Some(
            self.entry(agent)?
                .targets
                .iter()
                .map(|(k, _)| k.as_str())
                .collect(),
        )
    }

    /// One target (None for unknown paths).
    pub fn target(&self, agent: &str, target: &str) -> Option<&Target> {
        self.entry(agent)?
            .targets
            .iter()
            .find(|(k, _)| k == target)
            .map(|(_, t)| t)
    }

    /// The item keys of one group in source order (None for unknown paths
    /// or a non-group target).
    pub fn group_items(&self, agent: &str, group: &str) -> Option<Vec<&str>> {
        Some(self.target(agent, group)?.as_group()?.items())
    }

    /// Every deployable path in source order: `(agent, target, item)` —
    /// 2-level leaves carry `item: None`, 3-level leaves carry the item.
    pub fn paths(&self) -> Vec<(String, String, Option<String>)> {
        let mut out = Vec::new();
        for (agent, entry) in &self.agents {
            for (target, t) in &entry.targets {
                match t {
                    Target::Leaf(_) => out.push((agent.clone(), target.clone(), None)),
                    Target::Group(g) => {
                        for (item, _) in &g.items {
                            out.push((agent.clone(), target.clone(), Some(item.clone())));
                        }
                    }
                }
            }
        }
        out
    }

    /// The effective config of one deployable path (None for unknown
    /// paths): 2-level = `(agent, leaf, None)`, 3-level =
    /// `(agent, group, Some(item))`.
    pub fn effective(
        &self,
        agent: &str,
        target: &str,
        item: Option<&str>,
    ) -> Option<EffectiveConfig> {
        let entry = self.entry(agent)?;
        let t = self.target(agent, target)?;
        let (leaf, item) = match t {
            // An --item on a 2-level leaf never resolves (groups only).
            Target::Leaf(l) if item.is_none() => (l, None),
            Target::Group(g) => {
                let item = item?;
                (
                    g.items.iter().find(|(k, _)| k == item).map(|(_, l)| l)?,
                    Some(item.to_string()),
                )
            }
            _ => return None,
        };
        Some(EffectiveConfig {
            agent: agent.to_string(),
            target: target.to_string(),
            item,
            src: leaf.src.clone(),
            r#ref: leaf.r#ref.clone(),
            dst: entry.deploy.dst.clone(),
            mode: entry.deploy.mode.clone(),
            post_hook: leaf
                .post_hook
                .clone()
                .or_else(|| entry.deploy.post_hook.clone()),
        })
    }
}

/// Spec §12 semantic validation, one level per tier. Errors name the full
/// `agent → target[/item]` path and the reason.
fn validate(catalog: &AgentCatalog) -> Result<(), String> {
    if catalog.version != CURRENT_VERSION {
        return Err(format!(
            "unsupported catalog version {} (expected {CURRENT_VERSION})",
            catalog.version
        ));
    }
    if catalog.agents.is_empty() {
        return Err("agents must be non-empty".into());
    }
    for (agent, entry) in &catalog.agents {
        if !valid_name(agent) {
            return Err(format!(
                "invalid agent name {agent:?} (1..64 chars, no leading/trailing \
                 spaces, no control chars)"
            ));
        }
        if entry.targets.is_empty() {
            return Err(format!("agent {agent:?} has no targets"));
        }
        let deploy = &entry.deploy;
        if deploy.mode != "sync" && deploy.mode != "merge" {
            return Err(format!(
                "agent {agent:?}: deploy.mode {:?} (expected \"sync\" or \"merge\")",
                deploy.mode
            ));
        }
        if !valid_dst(&deploy.dst) {
            return Err(format!(
                "agent {agent:?}: deploy.dst {:?} must be a relative path without \"..\"",
                deploy.dst
            ));
        }
        // Spec §9.2: sync into the project root is forbidden (dst=="" —
        // and "." which also resolves to the root).
        if deploy.mode == "sync" && (deploy.dst.is_empty() || deploy.dst == ".") {
            return Err(format!(
                "agent {agent:?}: deploy.mode \"sync\" with dst {:?} \
                 (syncing the project root is forbidden)",
                deploy.dst
            ));
        }
        if let Some(hook) = &deploy.post_hook
            && !valid_post_hook(hook)
        {
            return Err(format!(
                "agent {agent:?}: deploy.post_hook {:?} must be a non-empty \
                 relative path without \"..\"",
                hook
            ));
        }
        for (target, t) in &entry.targets {
            if !valid_name(target) {
                return Err(format!(
                    "invalid target name {target:?} under agent {agent:?} \
                     (1..64 chars, no leading/trailing spaces, no control chars)"
                ));
            }
            match t {
                Target::Leaf(leaf) => validate_leaf(catalog, agent, target, None, leaf)?,
                Target::Group(group) => {
                    if group.items.is_empty() {
                        return Err(format!("agent {agent:?} target {target:?} has no items"));
                    }
                    for (item, leaf) in &group.items {
                        if !valid_name(item) {
                            return Err(format!(
                                "invalid item name {item:?} under agent {agent:?} \
                                 target {target:?} (1..64 chars, no leading/trailing \
                                 spaces, no control chars)"
                            ));
                        }
                        validate_leaf(catalog, agent, target, Some(item), leaf)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// The per-leaf checks (spec §12 rules 7, 10 + schema limits).
fn validate_leaf(
    _catalog: &AgentCatalog,
    agent: &str,
    target: &str,
    item: Option<&str>,
    leaf: &Leaf,
) -> Result<(), String> {
    let path = match item {
        Some(item) => format!("{agent:?} target {target:?} item {item:?}"),
        None => format!("{agent:?} target {target:?}"),
    };
    if leaf.src.is_empty() {
        return Err(format!("{path}: src must be non-empty"));
    }
    if leaf.src.chars().count() > SRC_MAX {
        return Err(format!("{path}: src exceeds {SRC_MAX} chars"));
    }
    if leaf.src.chars().any(char::is_control) {
        return Err(format!("{path}: src must not contain control chars"));
    }
    if let Some(r) = &leaf.r#ref
        && !valid_ref(r)
    {
        return Err(format!(
            "{path}: ref {r:?} must be non-empty, at most {REF_MAX} chars, \
             not start with \"-\", and contain no control chars"
        ));
    }
    if let Some(hook) = &leaf.post_hook
        && !valid_post_hook(hook)
    {
        return Err(format!(
            "{path}: post_hook {hook:?} must be a non-empty relative path without \"..\"",
        ));
    }
    Ok(())
}

/// Spec §12 rules 8/9 + schema `name`: 1..64 chars, no leading/trailing
/// whitespace, no control chars. Interior spaces and unicode are legal.
fn valid_name(k: &str) -> bool {
    let n = k.chars().count();
    (1..=NAME_MAX).contains(&n) && k.trim() == k && !k.chars().any(char::is_control)
}

/// Spec §12 rules 1/2 + schema: dst is relative (may be multi-component),
/// no `ParentDir` components (checked with `Path::components()`, never
/// `contains("..")`), at most 256 chars. Empty = the project root.
fn valid_dst(d: &str) -> bool {
    d.chars().count() <= DST_MAX
        && !Path::new(d).is_absolute()
        && !d.starts_with('/')
        && Path::new(d).components().all(|c| c != Component::ParentDir)
}

/// Spec §12 rules 3/4 + schema: non-empty relative repo path, no
/// `ParentDir`, at most 256 chars.
fn valid_post_hook(p: &str) -> bool {
    !p.is_empty()
        && p.chars().count() <= HOOK_MAX
        && !Path::new(p).is_absolute()
        && !p.starts_with('/')
        && Path::new(p).components().all(|c| c != Component::ParentDir)
}

/// Spec §12 rule 7 + schema: non-empty, not `-`-prefixed (a git option),
/// at most 256 chars, no control chars.
fn valid_ref(r: &str) -> bool {
    !r.is_empty()
        && !r.starts_with('-')
        && r.chars().count() <= REF_MAX
        && !r.chars().any(char::is_control)
}

/// The catalog path: `DUTABO_AGENT_CATALOG` → XDG → HOME/.config.
/// Resolved from env manually — no dirs/home crate (dependency lock).
pub fn catalog_path() -> PathBuf {
    if let Ok(env) = std::env::var(AGENT_CATALOG_ENV)
        && !env.trim().is_empty()
    {
        return PathBuf::from(env);
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(xdg).join("dutabo").join(CONFIG_FILE_NAME);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("dutabo")
            .join(CONFIG_FILE_NAME);
    }
    PathBuf::from(".config")
        .join("dutabo")
        .join(CONFIG_FILE_NAME)
}

/// Load the catalog once per run (the TUI's `r` re-invokes this). A
/// missing file is `Missing` — never an error.
pub fn load_catalog() -> (AgentCatalog, CatalogStatus) {
    let path = catalog_path();
    if !path.exists() {
        return (AgentCatalog::empty(), CatalogStatus::Missing(path));
    }
    match load_from(&path) {
        Ok(catalog) => (catalog, CatalogStatus::Loaded(path)),
        Err(reason) => (AgentCatalog::empty(), CatalogStatus::Invalid(path, reason)),
    }
}

/// Read + parse + validate one catalog file. Errors carry `<path>: <reason>`.
pub fn load_from(path: &Path) -> Result<AgentCatalog, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    AgentCatalog::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize env access with the other lib tests that mutate env vars.
    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The spec §6 example document (groups + leaves, insertion order
    /// that DIFFERS from sorted order at every level: Rockchip before
    /// Allwinner, AOSP before Linux before OpenHarmony, Rust last).
    const EXAMPLE: &str = r#"// ~/.config/dutabo/config.jsonc
{
  "version": 1,

  "agents": {
    "claude-code": {
      "deploy": { "dst": ".claude", "mode": "sync", "post_hook": "install.sh" },
      "targets": {
        "Rockchip": {
          "items": {
            "AOSP": { "src": "https://git.example.com/agent/claude/rk-aosp.git", "ref": "main" },
            "Linux": { "src": "https://git.example.com/agent/claude/rk-linux.git", "ref": "main" },
            "OpenHarmony": { "src": "https://git.example.com/agent/claude/rk-openharmony.git", "ref": "main" }
          }
        },
        "Allwinner": {
          "items": {
            "AOSP": { "src": "https://git.example.com/agent/claude/aw-aosp.git" },
            "Linux": { "src": "https://git.example.com/agent/claude/aw-linux.git" }
          }
        },
        "Chromium": { "src": "https://git.example.com/agent/claude/chromium.git", "ref": "main" },
        "ROS2":     { "src": "https://git.example.com/agent/claude/ros2.git", "ref": "main" },
        "Rust":     { "src": "https://git.example.com/agent/claude/rust.git", "ref": "main" }
      }
    },

    "pi.dev": {
      "deploy": { "dst": ".pi", "mode": "sync" },
      "targets": {
        "Rockchip": {
          "items": {
            "Linux": { "src": "https://git.example.com/agent/pi/rk-linux.git" }
          }
        },
        "Rust": { "src": "https://git.example.com/agent/pi/rust.git" }
      }
    },

    "dsh": {
      "deploy": { "dst": "", "mode": "merge" },
      "targets": {
        "Linux": { "src": "https://git.example.com/agent/dsh/linux.git" },
        "Rust":  { "src": "https://git.example.com/agent/dsh/rust.git" }
      }
    }
  }
}
"#;

    /// c1: the spec §6 example parses; SOURCE ORDER is preserved at every
    /// level (never sorted); groups and leaves discriminate correctly;
    /// `ref` and the agent-level post_hook survive.
    #[test]
    fn catalog_example_parses_order_preserved() {
        let catalog = AgentCatalog::parse(EXAMPLE).expect("example parses");
        assert_eq!(catalog.version, 1);
        assert_eq!(
            catalog.agents(),
            ["claude-code", "pi.dev", "dsh"],
            "agents keep source order"
        );
        let cc = catalog.entry("claude-code").expect("claude-code agent");
        assert_eq!(cc.deploy.dst, ".claude");
        assert_eq!(cc.deploy.mode, "sync");
        assert_eq!(
            catalog.targets("claude-code").unwrap(),
            ["Rockchip", "Allwinner", "Chromium", "ROS2", "Rust"],
            "targets keep source order"
        );
        // Rockchip is a Group; Chromium is a Leaf.
        assert!(
            catalog
                .target("claude-code", "Rockchip")
                .unwrap()
                .as_group()
                .is_some()
        );
        assert!(
            catalog
                .target("claude-code", "Chromium")
                .unwrap()
                .as_leaf()
                .is_some()
        );
        assert_eq!(
            catalog.group_items("claude-code", "Rockchip").unwrap(),
            ["AOSP", "Linux", "OpenHarmony"],
            "items keep source order"
        );
        let leaf = catalog
            .target("claude-code", "Chromium")
            .unwrap()
            .as_leaf()
            .unwrap();
        assert_eq!(
            leaf.src,
            "https://git.example.com/agent/claude/chromium.git"
        );
        assert_eq!(leaf.r#ref.as_deref(), Some("main"), "ref survives");
        // The agent-level post_hook is not a leaf field — it comes from
        // the deploy block via `effective`.
        assert_eq!(leaf.post_hook, None);
        assert_eq!(cc.deploy.post_hook.as_deref(), Some("install.sh"));
    }

    /// c2: unknown keys are REJECTED and NAMED at every v1 tier (root,
    /// deploy, leaf, group — `deny_unknown_fields`).
    #[test]
    fn unknown_keys_rejected() {
        let root_typo = EXAMPLE.replace("\"version\": 1", "\"version\": 1, \"agentsx\": {}");
        let err = AgentCatalog::parse(&root_typo).unwrap_err();
        assert!(err.contains("agentsx"), "root typo named: {err}");
        let deploy_typo = EXAMPLE.replace("\"mode\": \"sync\"", "\"modex\": \"sync\"");
        let err = AgentCatalog::parse(&deploy_typo).unwrap_err();
        assert!(err.contains("modex"), "deploy typo named: {err}");
        let leaf_typo = EXAMPLE.replace(
            "\"src\": \"https://git.example.com/agent/dsh/linux.git\"",
            "\"src\": \"https://git.example.com/agent/dsh/linux.git\", \"remot\": 1",
        );
        let err = AgentCatalog::parse(&leaf_typo).unwrap_err();
        assert!(err.contains("remot"), "leaf typo named: {err}");
        let group_typo = EXAMPLE.replace(
            "\"items\": {\n            \"Linux\": { \"src\": \"https://git.example.com/agent/pi/rk-linux.git\" }\n          }",
            "\"items\": {\n            \"Linux\": { \"src\": \"https://git.example.com/agent/pi/rk-linux.git\" }\n          }, \"itemz\": {}",
        );
        let err = AgentCatalog::parse(&group_typo).unwrap_err();
        assert!(err.contains("itemz"), "group typo named: {err}");
    }

    /// c3: the version gate — only 1 is supported and the number is named;
    /// a missing `version` (or the old v0 flat shape) is a schema error.
    #[test]
    fn version_gate() {
        let v2 = EXAMPLE.replace("\"version\": 1", "\"version\": 2");
        let err = AgentCatalog::parse(&v2).unwrap_err();
        assert!(err.contains("version"), "{err}");
        assert!(err.contains('2'), "{err}");
        let no_version = EXAMPLE.replace("\"version\": 1,\n", "");
        let err = AgentCatalog::parse(&no_version).unwrap_err();
        assert!(err.contains("version"), "{err}");
        // The v0 flat shape (no version wrapper) is NOT accepted anymore.
        let v0 = r#"{"claude-code": {"Rockchip": {"linux": {"src": "https://x/y.git", "post_hook": "install.sh", "dst": ".claude"}}}}"#;
        let err = AgentCatalog::parse(v0).unwrap_err();
        assert!(err.contains("version"), "v0 shape rejected: {err}");
    }

    /// c4: the ONLY two path shapes are Agent→Leaf and Agent→Group→Leaf —
    /// a group nested inside a group is rejected; targets that are
    /// neither leaf nor group are rejected; empty levels are rejected.
    #[test]
    fn only_two_path_shapes() {
        let nested = EXAMPLE.replace(
            "\"AOSP\": { \"src\": \"https://git.example.com/agent/claude/rk-aosp.git\", \"ref\": \"main\" },",
            "\"AOSP\": { \"items\": { \"inner\": { \"src\": \"https://x/y.git\" } } },",
        );
        let err = AgentCatalog::parse(&nested).unwrap_err();
        assert!(err.contains("items"), "nested group rejected: {err}");
        let neither = EXAMPLE.replace(
            "\"src\": \"https://git.example.com/agent/dsh/rust.git\"",
            "\"foo\": 1",
        );
        let err = AgentCatalog::parse(&neither).unwrap_err();
        assert!(
            err.contains("leaf") || err.contains("group"),
            "neither-shape named: {err}"
        );
        // Empty levels (schema minProperties 1).
        assert!(AgentCatalog::parse(r#"{"version": 1, "agents": {}}"#).is_err());
        let no_targets = EXAMPLE.replace(
            "\"targets\": {\n        \"Linux\":",
            "\"targets\": {},\n        \"targetsz\": {\n        \"Linux\":",
        );
        assert!(AgentCatalog::parse(&no_targets).is_err());
        let empty_items = EXAMPLE.replace(
            "\"items\": {\n            \"Linux\": { \"src\": \"https://git.example.com/agent/pi/rk-linux.git\" }\n          }",
            "\"items\": {}",
        );
        let err = AgentCatalog::parse(&empty_items).unwrap_err();
        assert!(err.contains("no items"), "{err}");
    }

    /// c5: `ref` is parsed AND validated — non-empty, not `-`-prefixed,
    /// ≤256, no control chars; valid branch/tag/commit shapes pass.
    #[test]
    fn ref_accepted_and_validated() {
        let with_ref = |r: &str| {
            EXAMPLE.replace(
                "\"src\": \"https://git.example.com/agent/dsh/linux.git\"",
                &format!(
                    "\"src\": \"https://git.example.com/agent/dsh/linux.git\", \"ref\": {r:?}"
                ),
            )
        };
        for bad in ["", "-x", "-", "--hard"] {
            let err = AgentCatalog::parse(&with_ref(bad)).unwrap_err();
            assert!(err.contains("ref"), "ref {bad:?} rejected: {err}");
        }
        let control = EXAMPLE.replace(
            "\"src\": \"https://git.example.com/agent/dsh/linux.git\"",
            "\"src\": \"https://git.example.com/agent/dsh/linux.git\", \"ref\": \"ma\\tin\"",
        );
        assert!(AgentCatalog::parse(&control).is_err());
        let long = with_ref(&"x".repeat(257));
        let err = AgentCatalog::parse(&long).unwrap_err();
        assert!(err.contains("ref"), "{err}");
        // Valid shapes: branch, tag, full commit sha, short sha.
        for ok in [
            "main",
            "v1.0.0",
            "release/2026.1",
            "abc123",
            &"f".repeat(40),
        ] {
            assert!(
                AgentCatalog::parse(&with_ref(ok)).is_ok(),
                "ref {ok:?} must parse"
            );
        }
    }

    /// c6: post_hook is OPTIONAL at both levels; the effective hook is
    /// `leaf.post_hook ?? agent.deploy.post_hook` (spec §8 precedence).
    #[test]
    fn post_hook_optional_and_precedence() {
        let no_hooks = EXAMPLE.replace("\"post_hook\": \"install.sh\"", "");
        let catalog = AgentCatalog::parse(&no_hooks).expect("no hooks parses");
        let e = catalog
            .effective("claude-code", "Chromium", None)
            .expect("leaf path");
        assert_eq!(e.post_hook, None, "no hook anywhere → None");
        // Leaf hook overrides the agent hook.
        let leaf_hook = EXAMPLE.replace(
            "\"src\": \"https://git.example.com/agent/claude/chromium.git\", \"ref\": \"main\"",
            "\"src\": \"https://git.example.com/agent/claude/chromium.git\", \"ref\": \"main\", \"post_hook\": \"leaf-install.sh\"",
        );
        let catalog = AgentCatalog::parse(&leaf_hook).expect("leaf hook parses");
        let e = catalog
            .effective("claude-code", "Chromium", None)
            .expect("leaf path");
        assert_eq!(
            e.post_hook.as_deref(),
            Some("leaf-install.sh"),
            "leaf.post_hook wins"
        );
        // pi.dev has NO agent hook — only its leaves may carry one.
        let e = catalog.effective("pi.dev", "Rust", None).expect("pi path");
        assert_eq!(e.post_hook, None);
        // claude-code's agent hook applies when no leaf hook exists (the
        // spec §8 fallback).
        let full = AgentCatalog::parse(EXAMPLE).expect("example parses");
        assert_eq!(
            full.effective("claude-code", "Chromium", None)
                .unwrap()
                .post_hook
                .as_deref(),
            Some("install.sh"),
            "agent.deploy.post_hook applies"
        );
        // An EMPTY post_hook is schema-invalid (minLength 1).
        let empty_hook = EXAMPLE.replace(
            "\"src\": \"https://git.example.com/agent/dsh/linux.git\"",
            "\"src\": \"https://git.example.com/agent/dsh/linux.git\", \"post_hook\": \"\"",
        );
        let err = AgentCatalog::parse(&empty_hook).unwrap_err();
        assert!(err.contains("post_hook"), "{err}");
    }

    /// c7: the mode contract FLIPS (spec §9.2 + schema `allOf`):
    /// `dst == ""` ⇒ `mode == "merge"` — sync with an empty dst is now
    /// INVALID; merge with a non-empty dst is now LEGAL.
    #[test]
    fn mode_contract_flip() {
        // sync + dst "" → invalid (was legal).
        let sync_root = EXAMPLE.replace(
            "\"deploy\": { \"dst\": \"\", \"mode\": \"merge\" }",
            "\"deploy\": { \"dst\": \"\", \"mode\": \"sync\" }",
        );
        let err = AgentCatalog::parse(&sync_root).unwrap_err();
        assert!(err.contains("sync"), "{err}");
        // merge + non-empty dst → legal (was invalid).
        let merge_subdir = EXAMPLE.replace(
            "\"deploy\": { \"dst\": \"\", \"mode\": \"merge\" }",
            "\"deploy\": { \"dst\": \".dsh\", \"mode\": \"merge\" }",
        );
        let catalog = AgentCatalog::parse(&merge_subdir).expect("merge+dst parses");
        assert_eq!(
            catalog.effective("dsh", "Linux", None).expect("path").dst,
            ".dsh"
        );
        // sync + non-empty dst stays legal; unknown modes rejected.
        assert!(AgentCatalog::parse(EXAMPLE).is_ok());
        let bad_mode = EXAMPLE.replace("\"mode\": \"merge\"", "\"mode\": \"rsync\"");
        let err = AgentCatalog::parse(&bad_mode).unwrap_err();
        assert!(err.contains("mode"), "{err}");
        // sync + dst "." also resolves to the root → forbidden.
        let sync_dot = EXAMPLE.replace(
            "\"deploy\": { \"dst\": \"\", \"mode\": \"merge\" }",
            "\"deploy\": { \"dst\": \".\", \"mode\": \"sync\" }",
        );
        assert!(AgentCatalog::parse(&sync_dot).is_err());
    }

    /// c8: dst semantics (spec §12 rules 1/2): multi-component relative
    /// paths are LEGAL now; absolute and `ParentDir` components are
    /// rejected via `Path::components()` (not `contains("..")`).
    #[test]
    fn dst_semantics() {
        let with_dst = |d: &str| {
            EXAMPLE.replace(
                "\"deploy\": { \"dst\": \"\", \"mode\": \"merge\" }",
                &format!("\"deploy\": {{ \"dst\": {d:?}, \"mode\": \"merge\" }}"),
            )
        };
        for ok in [
            "",
            ".claude",
            ".pi",
            "agent-repo",
            "a/b",
            "config/agents/cc",
        ] {
            assert!(
                AgentCatalog::parse(&with_dst(ok)).is_ok(),
                "dst {ok:?} must parse"
            );
        }
        for bad in ["/abs", "../x", "a/../../b", ".."] {
            let err = AgentCatalog::parse(&with_dst(bad)).unwrap_err();
            assert!(err.contains("dst"), "dst {bad:?} rejected: {err}");
        }
        let long = with_dst(&"x".repeat(257));
        assert!(AgentCatalog::parse(&long).is_err(), "dst >256 rejected");
    }

    /// c9: name rules (spec §12 rules 8/9 + schema `name`): interior
    /// spaces and unicode are LEGAL; leading/trailing whitespace, control
    /// chars, empty, and >64 chars are rejected. (Applied to agent,
    /// target, and item keys.)
    #[test]
    fn name_rules() {
        let agent = |n: &str| EXAMPLE.replace("\"claude-code\"", &format!("{n:?}"));
        for ok in ["my agent", "Résumé", "Rockchip", "pi.dev", "a b c"] {
            assert!(
                AgentCatalog::parse(&agent(ok)).is_ok(),
                "agent name {ok:?} must parse"
            );
        }
        for bad in [" leading", "trailing ", "\t tab", "bad\nname"] {
            let err = AgentCatalog::parse(&agent(bad)).unwrap_err();
            assert!(err.contains("agent name"), "agent {bad:?} rejected: {err}");
        }
        // DEL (U+007F) is a control char too — escaped as \\u007f (Rust's
        // {:?} escape \\u{7f} is not valid JSON).
        let del = EXAMPLE.replace("\"claude-code\"", "\"x\\u007fy\"");
        let err = AgentCatalog::parse(&del).unwrap_err();
        assert!(err.contains("agent name"), "DEL rejected: {err}");
        let long = agent(&"x".repeat(65));
        assert!(AgentCatalog::parse(&long).is_err(), ">64 chars rejected");
        // The same rule applies to target and item keys.
        let bad_target = EXAMPLE.replace("\"Rust\"", "\" Rust\"");
        let err = AgentCatalog::parse(&bad_target).unwrap_err();
        assert!(err.contains("target name"), "{err}");
        let bad_item = EXAMPLE.replace("\"AOSP\"", "\"AOSP \"");
        let err = AgentCatalog::parse(&bad_item).unwrap_err();
        assert!(err.contains("item name"), "{err}");
        // A 64-char name is exactly legal.
        let n64 = "x".repeat(64);
        assert!(AgentCatalog::parse(&agent(&n64)).is_ok());
    }

    /// c10: src rules — the scheme whitelist is GONE: any non-empty ≤2048
    /// string without control chars parses (git failures surface at clone
    /// time). Empty, >2048, and control chars are rejected.
    #[test]
    fn src_rules() {
        let with_src = |s: &str| {
            EXAMPLE.replace(
                "\"src\": \"https://git.example.com/agent/dsh/linux.git\"",
                &format!("\"src\": {s:?}"),
            )
        };
        for ok in [
            "https://x/y.git",
            "ftp://x/y.git",
            "git@github.com:x/y.git",
            "/srv/local/mirror",
            "plain-relative-path",
        ] {
            assert!(
                AgentCatalog::parse(&with_src(ok)).is_ok(),
                "src {ok:?} must parse (no scheme whitelist)"
            );
        }
        for bad in ["", "ht\ttps://x/y.git", "a\nb"] {
            assert!(AgentCatalog::parse(&with_src(bad)).is_err(), "src {bad:?}");
        }
        let long = with_src(&"x".repeat(2049));
        assert!(AgentCatalog::parse(&long).is_err(), "src >2048 rejected");
    }

    /// c11: the effective-config pin (spec §8): src/ref from the leaf;
    /// dst/mode from the agent deploy block; post_hook precedence
    /// leaf > agent — for BOTH path shapes.
    #[test]
    fn effective_config_pin() {
        let catalog = AgentCatalog::parse(EXAMPLE).expect("example parses");
        // 2-level: dsh / Linux.
        let e = catalog
            .effective("dsh", "Linux", None)
            .expect("2-level path");
        assert_eq!(
            e,
            EffectiveConfig {
                agent: "dsh".into(),
                target: "Linux".into(),
                item: None,
                src: "https://git.example.com/agent/dsh/linux.git".into(),
                r#ref: None,
                dst: "".into(),
                mode: "merge".into(),
                post_hook: None,
            }
        );
        assert_eq!(e.path(), "dsh/Linux");
        // 3-level: claude-code / Rockchip / AOSP.
        let e = catalog
            .effective("claude-code", "Rockchip", Some("AOSP"))
            .expect("3-level path");
        assert_eq!(
            (e.agent.as_str(), e.target.as_str(), e.item.as_deref()),
            ("claude-code", "Rockchip", Some("AOSP"))
        );
        assert_eq!(e.src, "https://git.example.com/agent/claude/rk-aosp.git");
        assert_eq!(e.r#ref.as_deref(), Some("main"));
        assert_eq!(e.dst, ".claude");
        assert_eq!(e.mode, "sync");
        assert_eq!(e.post_hook.as_deref(), Some("install.sh"));
        assert_eq!(e.path(), "claude-code/Rockchip/AOSP");
        // Unknown paths → None.
        assert!(catalog.effective("nope", "Linux", None).is_none());
        assert!(catalog.effective("dsh", "nope", None).is_none());
        assert!(
            catalog.effective("claude-code", "Rockchip", None).is_none(),
            "a group is not deployable without an item"
        );
        assert!(
            catalog
                .effective("claude-code", "Rockchip", Some("nope"))
                .is_none()
        );
        // An item on a 2-level LEAF never resolves (groups only).
        assert!(catalog.effective("dsh", "Linux", Some("x")).is_none());
    }

    /// c12: `paths()` enumerates every deployable path in source order —
    /// 2-level leaves carry `None`, 3-level leaves their item.
    #[test]
    fn paths_enumeration_ordered() {
        let catalog = AgentCatalog::parse(EXAMPLE).expect("example parses");
        let paths = catalog.paths();
        assert_eq!(
            paths,
            [
                ("claude-code", "Rockchip", Some("AOSP")),
                ("claude-code", "Rockchip", Some("Linux")),
                ("claude-code", "Rockchip", Some("OpenHarmony")),
                ("claude-code", "Allwinner", Some("AOSP")),
                ("claude-code", "Allwinner", Some("Linux")),
                ("claude-code", "Chromium", None),
                ("claude-code", "ROS2", None),
                ("claude-code", "Rust", None),
                ("pi.dev", "Rockchip", Some("Linux")),
                ("pi.dev", "Rust", None),
                ("dsh", "Linux", None),
                ("dsh", "Rust", None),
            ]
            .map(|(a, t, i)| (a.to_string(), t.to_string(), i.map(String::from)))
            .to_vec()
        );
    }

    /// c13: path resolution precedence: `DUTABO_AGENT_CATALOG` > XDG >
    /// `HOME/.config` — the file name is `config.jsonc` at every tier.
    #[test]
    fn catalog_path_prefers_env_then_xdg_then_home() {
        let _guard = lock_env();
        unsafe {
            std::env::remove_var("DUTABO_AGENT_CATALOG");
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("HOME");
        }
        unsafe {
            std::env::set_var("DUTABO_AGENT_CATALOG", "/opt/team/config.jsonc");
        }
        assert_eq!(
            catalog_path(),
            std::path::PathBuf::from("/opt/team/config.jsonc")
        );
        unsafe {
            std::env::remove_var("DUTABO_AGENT_CATALOG");
            std::env::set_var("XDG_CONFIG_HOME", "/xdg");
        }
        assert_eq!(
            catalog_path(),
            std::path::PathBuf::from("/xdg/dutabo/config.jsonc")
        );
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::set_var("HOME", "/home/u");
        }
        assert_eq!(
            catalog_path(),
            std::path::PathBuf::from("/home/u/.config/dutabo/config.jsonc")
        );
    }

    /// c14: a missing catalog file is `CatalogStatus::Missing(path)` — NOT
    /// an error.
    #[test]
    fn missing_catalog_is_missing_not_error() {
        let _guard = lock_env();
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("config.jsonc");
        unsafe {
            std::env::set_var("DUTABO_AGENT_CATALOG", path.to_str().unwrap());
        }
        let (catalog, status) = load_catalog();
        assert!(catalog.agents.is_empty());
        assert_eq!(status, CatalogStatus::Missing(path));
    }

    /// c15: THE USER'S REAL config (verbatim shape — v1 targets with
    /// groups + leaves) parses: `dsh` uses `dst:""` + `mode:"merge"`;
    /// leaves carry `ref:"main"`; source order preserved.
    #[test]
    fn user_real_config_shape_parses() {
        let text = r#"// ~/.config/dutabo/config.jsonc — 项目级 Agent 配置注册表 
// Schema v1: https://dutabo.dev/schema/agent-config-v1.json
{
  "version": 1,
  "agents": {
    "claude-code": {
      "deploy": { "dst": ".claude", "mode": "sync", "post_hook": "install.sh" },
      "targets": {
        "Rockchip": { "items": {
          "AOSP": { "src": "https://git.example.com/agent/claude/rk-aosp.git", "ref": "main" },
          "Linux": { "src": "https://git.example.com/agent/claude/rk-linux.git", "ref": "main" }
        } },
        "Allwinner": { "items": {
          "AOSP": { "src": "https://git.example.com/agent/claude/aw-aosp.git" }
        } },
        "Chromium": { "src": "https://git.example.com/agent/claude/chromium.git", "ref": "main" },
        "ROS2": { "src": "https://git.example.com/agent/claude/ros2.git", "ref": "main" },
        "Rust": { "src": "https://git.example.com/agent/claude/rust.git", "ref": "main" }
      }
    },
    "pi.dev": {
      "deploy": { "dst": ".pi", "mode": "sync" },
      "targets": {
        "Rockchip": { "items": {
          "Linux": { "src": "https://git.example.com/agent/pi/rk-linux.git" }
        } },
        "Rust": { "src": "https://git.example.com/agent/pi/rust.git" }
      }
    },
    "dsh": {
      "deploy": { "dst": "", "mode": "merge" },
      "targets": {
        "Linux": { "src": "https://git.example.com/agent/dsh/linux.git" },
        "Rust": { "src": "https://git.example.com/agent/dsh/rust.git" }
      }
    }
  }
}
"#;
        let catalog = AgentCatalog::parse(text).expect("the real v1 shape parses");
        assert_eq!(catalog.agents(), ["claude-code", "pi.dev", "dsh"]);
        let dsh = catalog.effective("dsh", "Linux", None).expect("path");
        assert_eq!(dsh.dst, "");
        assert_eq!(dsh.mode, "merge");
        let cc = catalog
            .effective("claude-code", "Rockchip", Some("Linux"))
            .expect("path");
        assert_eq!(cc.r#ref.as_deref(), Some("main"));
        assert_eq!(cc.dst, ".claude");
        assert_eq!(cc.mode, "sync");
    }

    /// c16: a document that parses but is semantically invalid names the
    /// FULL path (agent → target → item) in the error — the shared
    /// validation runs AFTER schema deserialization for both path shapes.
    #[test]
    fn semantic_errors_name_full_path() {
        let bad_src = EXAMPLE.replace(
            "\"src\": \"https://git.example.com/agent/claude/aw-aosp.git\"",
            "\"src\": \"\"",
        );
        let err = AgentCatalog::parse(&bad_src).unwrap_err();
        assert!(
            err.contains("claude-code") && err.contains("Allwinner") && err.contains("AOSP"),
            "full 3-level path named: {err}"
        );
        let bad_ref = EXAMPLE.replace(
            "\"src\": \"https://git.example.com/agent/dsh/rust.git\"",
            "\"src\": \"https://git.example.com/agent/dsh/rust.git\", \"ref\": \"-x\"",
        );
        let err = AgentCatalog::parse(&bad_ref).unwrap_err();
        assert!(
            err.contains("dsh") && err.contains("Rust"),
            "full 2-level path named: {err}"
        );
    }
}
