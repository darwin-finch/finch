use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOperation {
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "name", rename_all = "snake_case")]
pub enum ResourceRoot {
    Workspace,
    Project,
    TaskOutput,
    /// An explicit whole-machine authority root. Constructing or granting this
    /// root is host policy; it is never inferred from an absolute string.
    HostMachine,
    Named(String),
}

impl fmt::Display for ResourceRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Workspace => f.write_str("workspace"),
            Self::Project => f.write_str("project"),
            Self::TaskOutput => f.write_str("task.output"),
            Self::HostMachine => f.write_str("host-machine"),
            Self::Named(name) => f.write_str(name),
        }
    }
}

/// A normalized pattern relative to an immutable resource root.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FileSelector {
    pub root: ResourceRoot,
    pub pattern: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SelectorError {
    #[error("resource selector is empty")]
    Empty,
    #[error("absolute paths are not valid capability selectors")]
    AbsolutePath,
    #[error("parent traversal is not valid in capability selectors")]
    ParentTraversal,
    #[error("unknown resource-root template: {0}")]
    UnknownRoot(String),
    #[error("invalid recursive wildcard segment: {0}")]
    InvalidRecursiveWildcard(String),
    #[error("selectors have different resource roots")]
    DifferentRoots,
    #[error("selector intersection cannot be represented safely")]
    IndeterminateIntersection,
    #[error("selector template argument {0} is missing or is not a path")]
    InvalidTemplateArgument(usize),
    #[error("selector template argument {0} exceeds its declared bound")]
    TemplateArgumentOutOfBounds(usize),
    #[error("runtime path values cannot contain wildcard syntax")]
    WildcardInRuntimePath,
    #[error("capability selectors must use '/' as the portable separator")]
    InvalidSeparator,
    #[error("network selector template argument {0} is missing or has the wrong type")]
    InvalidNetworkTemplateArgument(usize),
    #[error("network selector template argument {0} exceeds its declared bound")]
    NetworkTemplateArgumentOutOfBounds(usize),
    #[error("process selector template argument {0} is missing or has the wrong type")]
    InvalidProcessTemplateArgument(usize),
    #[error("process selector template argument {0} exceeds its declared bound")]
    ProcessTemplateArgumentOutOfBounds(usize),
    #[error("program selector template argument {0} is missing or has the wrong type")]
    InvalidProgramTemplateArgument(usize),
    #[error("program selector template argument {0} exceeds its declared bound")]
    ProgramTemplateArgumentOutOfBounds(usize),
}

/// A deliberately small expression language for argument-dependent file
/// capabilities. It is data, not an interpolated string: each argument must be
/// a refined `path` value and the result must remain inside `upper_bound`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FileSelectorTemplate {
    pub root: ResourceRoot,
    pub parts: Vec<FileSelectorTemplatePart>,
    pub upper_bound: FileSelector,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "part", rename_all = "snake_case")]
pub enum FileSelectorTemplatePart {
    Literal { relative: String },
    Argument { index: usize, bound: FileSelector },
}

/// Argument-dependent network authority.  The host and port are taken from
/// typed call arguments, checked against the descriptor's upper bound, then
/// materialized as a concrete `Network` selector before authorization.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NetworkSelectorTemplate {
    pub host_argument: usize,
    pub port_argument: usize,
    /// `*` means any hostname; an empty list also means any hostname so a
    /// descriptor can express an intentionally broad but still concrete-at-
    /// call-time request.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
    /// Empty means any valid TCP port.
    #[serde(default)]
    pub allowed_ports: Vec<u16>,
}

/// Argument-dependent process authority. The executable is materialized from
/// a typed string argument before authorization; program arguments do not
/// become authority selectors.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProcessSelectorTemplate {
    pub executable_argument: usize,
    /// Empty means any executable. Production descriptors should prefer an
    /// explicit allowlist, while a user can intentionally grant the broad
    /// concrete request at an approval boundary.
    #[serde(default)]
    pub allowed_executables: Vec<String>,
}

/// Argument-dependent proposal authority. The artifact language is concrete
/// at the host boundary, so one proposal language cannot authorize another.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProgramSelectorTemplate {
    pub language_argument: usize,
    #[serde(default)]
    pub allowed_languages: Vec<String>,
}

impl NetworkSelectorTemplate {
    pub fn instantiate(
        &self,
        arguments: &[super::types::TypedValue],
    ) -> Result<(String, u16), SelectorError> {
        let Some(super::types::TypedValue::String(host)) = arguments.get(self.host_argument) else {
            return Err(SelectorError::InvalidNetworkTemplateArgument(
                self.host_argument,
            ));
        };
        if host.is_empty()
            || (!self.allowed_hosts.is_empty()
                && !self
                    .allowed_hosts
                    .iter()
                    .any(|allowed| allowed == "*" || allowed == host))
        {
            return Err(SelectorError::NetworkTemplateArgumentOutOfBounds(
                self.host_argument,
            ));
        }
        let Some(super::types::TypedValue::Int(port)) = arguments.get(self.port_argument) else {
            return Err(SelectorError::InvalidNetworkTemplateArgument(
                self.port_argument,
            ));
        };
        let port = u16::try_from(*port)
            .map_err(|_| SelectorError::NetworkTemplateArgumentOutOfBounds(self.port_argument))?;
        if !self.allowed_ports.is_empty() && !self.allowed_ports.contains(&port) {
            return Err(SelectorError::NetworkTemplateArgumentOutOfBounds(
                self.port_argument,
            ));
        }
        Ok((host.clone(), port))
    }
}

impl ProcessSelectorTemplate {
    pub fn instantiate(
        &self,
        arguments: &[super::types::TypedValue],
    ) -> Result<String, SelectorError> {
        let Some(super::types::TypedValue::String(executable)) =
            arguments.get(self.executable_argument)
        else {
            return Err(SelectorError::InvalidProcessTemplateArgument(
                self.executable_argument,
            ));
        };
        if executable.is_empty()
            || (!self.allowed_executables.is_empty()
                && !self.allowed_executables.contains(executable))
        {
            return Err(SelectorError::ProcessTemplateArgumentOutOfBounds(
                self.executable_argument,
            ));
        }
        Ok(executable.clone())
    }
}

impl ProgramSelectorTemplate {
    pub fn instantiate(
        &self,
        arguments: &[super::types::TypedValue],
    ) -> Result<String, SelectorError> {
        let Some(super::types::TypedValue::String(language)) =
            arguments.get(self.language_argument)
        else {
            return Err(SelectorError::InvalidProgramTemplateArgument(
                self.language_argument,
            ));
        };
        let language = language.trim().to_ascii_lowercase();
        if language.is_empty()
            || (!self.allowed_languages.is_empty()
                && !self
                    .allowed_languages
                    .iter()
                    .any(|allowed| allowed == &language))
        {
            return Err(SelectorError::ProgramTemplateArgumentOutOfBounds(
                self.language_argument,
            ));
        }
        Ok(language)
    }
}

impl FileSelectorTemplate {
    pub fn instantiate(
        &self,
        arguments: &[super::types::TypedValue],
    ) -> Result<FileSelector, SelectorError> {
        if self.upper_bound.root != self.root {
            return Err(SelectorError::DifferentRoots);
        }
        let mut components = Vec::new();
        for part in &self.parts {
            let relative = match part {
                FileSelectorTemplatePart::Literal { relative } => {
                    let relative = normalize_relative(relative)?;
                    if relative.contains(['*', '?']) {
                        return Err(SelectorError::WildcardInRuntimePath);
                    }
                    relative
                }
                FileSelectorTemplatePart::Argument { index, bound } => {
                    let Some(super::types::TypedValue::Path { selector, relative }) =
                        arguments.get(*index)
                    else {
                        return Err(SelectorError::InvalidTemplateArgument(*index));
                    };
                    if selector.root != self.root
                        || bound.root != self.root
                        || !bound.contains_selector(selector)
                        || !selector.matches(relative)
                    {
                        return Err(SelectorError::TemplateArgumentOutOfBounds(*index));
                    }
                    let relative = normalize_relative(relative)?;
                    if relative.contains(['*', '?']) {
                        return Err(SelectorError::WildcardInRuntimePath);
                    }
                    relative
                }
            };
            if relative != "**" {
                components.push(relative);
            }
        }
        let exact = FileSelector {
            root: self.root.clone(),
            pattern: if components.is_empty() {
                "**".to_string()
            } else {
                components.join("/")
            },
        };
        if !self.upper_bound.contains_selector(&exact) {
            return Err(SelectorError::IndeterminateIntersection);
        }
        Ok(exact)
    }
}

impl FileSelector {
    pub fn parse(input: &str) -> Result<Self, SelectorError> {
        let input = input.trim();
        if input.is_empty() {
            return Err(SelectorError::Empty);
        }
        let (root, relative) = if let Some(rest) = input.strip_prefix("${") {
            let (root, relative) = rest
                .split_once('}')
                .ok_or_else(|| SelectorError::UnknownRoot(input.to_string()))?;
            let root = match root {
                "workspace" => ResourceRoot::Workspace,
                "project" => ResourceRoot::Project,
                "task.output" => ResourceRoot::TaskOutput,
                "host" | "host-machine" => ResourceRoot::HostMachine,
                name if !name.is_empty() => ResourceRoot::Named(name.to_string()),
                _ => return Err(SelectorError::UnknownRoot(root.to_string())),
            };
            (root, relative.trim_start_matches('/'))
        } else {
            (
                ResourceRoot::Workspace,
                input.strip_prefix("./").unwrap_or(input),
            )
        };
        let pattern = normalize_relative(relative)?;
        validate_pattern(&pattern)?;
        Ok(Self { root, pattern })
    }

    pub fn matches(&self, relative_path: &str) -> bool {
        let Ok(path) = normalize_relative(relative_path) else {
            return false;
        };
        let pattern = self.pattern.split('/').collect::<Vec<_>>();
        let path = path.split('/').collect::<Vec<_>>();
        match_segments(&pattern, &path)
    }

    /// Returns true only when containment is proven by the restricted selector
    /// algebra. False may mean either disjoint or not provable.
    pub fn contains_selector(&self, requested: &Self) -> bool {
        if self.root != requested.root {
            return false;
        }
        if self.pattern == requested.pattern || self.pattern == "**" {
            return true;
        }
        let Some(prefix) = self.pattern.strip_suffix("/**") else {
            return false;
        };
        requested.pattern == prefix
            || requested
                .pattern
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with('/'))
    }

    pub fn intersection(&self, other: &Self) -> Result<Self, SelectorError> {
        if self.root != other.root {
            return Err(SelectorError::DifferentRoots);
        }
        if self.contains_selector(other) {
            return Ok(other.clone());
        }
        if other.contains_selector(self) {
            return Ok(self.clone());
        }
        Err(SelectorError::IndeterminateIntersection)
    }
}

impl fmt::Display for FileSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "${{{}}}/{}", self.root, self.pattern)
    }
}

fn normalize_relative(input: &str) -> Result<String, SelectorError> {
    if input.contains('\\') {
        return Err(SelectorError::InvalidSeparator);
    }
    if input.is_empty() || input == "." {
        return Ok("**".to_string());
    }
    let path = Path::new(input);
    if path.is_absolute() {
        return Err(SelectorError::AbsolutePath);
    }
    let mut segments = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(segment) => {
                let segment = segment.to_string_lossy();
                if segment == ".." {
                    return Err(SelectorError::ParentTraversal);
                }
                segments.push(segment.into_owned());
            }
            Component::ParentDir => return Err(SelectorError::ParentTraversal),
            Component::RootDir | Component::Prefix(_) => return Err(SelectorError::AbsolutePath),
        }
    }
    if segments.is_empty() {
        Ok("**".to_string())
    } else {
        Ok(segments.join("/"))
    }
}

fn validate_pattern(pattern: &str) -> Result<(), SelectorError> {
    for segment in pattern.split('/') {
        if segment.contains("**") && segment != "**" {
            return Err(SelectorError::InvalidRecursiveWildcard(segment.to_string()));
        }
    }
    Ok(())
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => {
            match_segments(rest, path) || (!path.is_empty() && match_segments(pattern, &path[1..]))
        }
        Some((segment, rest)) => {
            let Some((path_segment, path_rest)) = path.split_first() else {
                return false;
            };
            match_component(segment, path_segment) && match_segments(rest, path_rest)
        }
    }
}

fn match_component(pattern: &str, value: &str) -> bool {
    let pattern = pattern.chars().collect::<Vec<_>>();
    let value = value.chars().collect::<Vec<_>>();
    let mut table = vec![vec![false; value.len() + 1]; pattern.len() + 1];
    table[0][0] = true;
    for p in 1..=pattern.len() {
        if pattern[p - 1] == '*' {
            table[p][0] = table[p - 1][0];
        }
        for v in 1..=value.len() {
            table[p][v] = match pattern[p - 1] {
                '*' => table[p - 1][v] || table[p][v - 1],
                '?' => table[p - 1][v - 1],
                literal => literal == value[v - 1] && table[p - 1][v - 1],
            };
        }
    }
    table[pattern.len()][value.len()]
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityKind {
    VmRead,
    VmWrite,
    FileRead,
    FileWrite,
    NetworkConnect,
    AutomationInspect,
    AutomationWrite,
    AgentSpawn,
    AgentAwait,
    AgentPoll,
    AgentCancel,
    ProcessRun,
    SessionEmit,
    MemoryRead,
    MemoryWrite,
    MemoryConsolidate,
    ScheduleCreate,
    ScheduleRead,
    ScheduleManage,
    ProgramInvoke,
    UnsafeMemory,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ResourceSelector {
    None,
    File {
        selector: FileSelector,
    },
    FileTemplate {
        template: FileSelectorTemplate,
    },
    NetworkTemplate {
        template: NetworkSelectorTemplate,
    },
    Network {
        host: String,
        ports: Vec<u16>,
    },
    Automation {
        application: Option<String>,
    },
    Agent {
        providers: Vec<String>,
        max_depth: u16,
        max_children: u16,
    },
    Process {
        executables: Vec<String>,
    },
    ProcessTemplate {
        template: ProcessSelectorTemplate,
    },
    Program {
        languages: Vec<String>,
    },
    ProgramTemplate {
        template: ProgramSelectorTemplate,
    },
    Memory {
        tree: String,
        path: String,
    },
    Schedule {
        policy: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CapabilityRequirement {
    pub capability: CapabilityKind,
    pub selector: ResourceSelector,
}

impl CapabilityRequirement {
    pub fn file(operation: FileOperation, selector: FileSelector) -> Self {
        Self {
            capability: match operation {
                FileOperation::Read => CapabilityKind::FileRead,
                FileOperation::Write => CapabilityKind::FileWrite,
            },
            selector: ResourceSelector::File { selector },
        }
    }

    /// Whether this grant covers the requested capability.
    pub fn covers(&self, requested: &Self) -> bool {
        if self.capability != requested.capability {
            return false;
        }
        match (&self.selector, &requested.selector) {
            (ResourceSelector::None, ResourceSelector::None) => true,
            // A file line or CSV record cursor can only be minted by its
            // path-scoped open request. Once it exists, follow-up reads and
            // close calls carry the unforgeable resource rather than a path
            // string, so the originating FileRead grant safely covers their
            // unscoped static selector.
            (ResourceSelector::File { .. }, ResourceSelector::None)
                if requested.capability == CapabilityKind::FileRead =>
            {
                true
            }
            // `network-send` is authorized by its opaque socket resource. A
            // socket can only be obtained from an already-authorized concrete
            // `network-connect`, and the host checks its generation/handle on
            // every send. Its static signature therefore carries `None`
            // rather than repeating an unforgeable endpoint inside source.
            (ResourceSelector::Network { .. }, ResourceSelector::None) => true,
            (
                ResourceSelector::File { selector: grant },
                ResourceSelector::File { selector: request },
            ) => grant.contains_selector(request),
            (
                ResourceSelector::File { selector: grant },
                ResourceSelector::FileTemplate { template },
            ) => grant.contains_selector(&template.upper_bound),
            (
                ResourceSelector::FileTemplate { template: grant },
                ResourceSelector::FileTemplate { template: request },
            ) => grant == request,
            (
                ResourceSelector::Network {
                    host: grant_host,
                    ports: grant_ports,
                },
                ResourceSelector::Network {
                    host: request_host,
                    ports: request_ports,
                },
            ) => {
                (grant_host == "*" || grant_host == request_host)
                    && (grant_ports.is_empty()
                        || request_ports.iter().all(|port| grant_ports.contains(port)))
            }
            (
                ResourceSelector::Network {
                    host: grant_host,
                    ports: grant_ports,
                },
                ResourceSelector::NetworkTemplate { template },
            ) => {
                let hosts_covered = template.allowed_hosts.is_empty()
                    || template
                        .allowed_hosts
                        .iter()
                        .all(|host| grant_host == "*" || (host != "*" && grant_host == host));
                let ports_covered = template.allowed_ports.is_empty()
                    || grant_ports.is_empty()
                    || template
                        .allowed_ports
                        .iter()
                        .all(|port| grant_ports.contains(port));
                hosts_covered && ports_covered
            }
            (
                ResourceSelector::NetworkTemplate { template: grant },
                ResourceSelector::NetworkTemplate { template: request },
            ) => grant == request,
            (
                ResourceSelector::Automation { application: grant },
                ResourceSelector::Automation {
                    application: request,
                },
            ) => grant.is_none() || grant == request,
            (
                ResourceSelector::Agent {
                    providers: grant_providers,
                    max_depth: grant_depth,
                    max_children: grant_children,
                },
                ResourceSelector::Agent {
                    providers: request_providers,
                    max_depth: request_depth,
                    max_children: request_children,
                },
            ) => {
                (grant_providers.is_empty()
                    || request_providers
                        .iter()
                        .all(|provider| grant_providers.contains(provider)))
                    && grant_depth >= request_depth
                    && grant_children >= request_children
            }
            (
                ResourceSelector::Process { executables: grant },
                ResourceSelector::Process {
                    executables: request,
                },
            ) => grant.is_empty() || request.iter().all(|item| grant.contains(item)),
            (
                ResourceSelector::Process { executables: grant },
                ResourceSelector::ProcessTemplate { template },
            ) => template
                .allowed_executables
                .is_empty()
                .then_some(grant.is_empty())
                .unwrap_or_else(|| {
                    grant.is_empty()
                        || template
                            .allowed_executables
                            .iter()
                            .all(|executable| grant.contains(executable))
                }),
            (
                ResourceSelector::ProcessTemplate { template: grant },
                ResourceSelector::ProcessTemplate { template: request },
            ) => grant == request,
            (
                ResourceSelector::Program { languages: grant },
                ResourceSelector::Program { languages: request },
            ) => grant.is_empty() || request.iter().all(|language| grant.contains(language)),
            (
                ResourceSelector::Program { languages: grant },
                ResourceSelector::ProgramTemplate { template },
            ) => template
                .allowed_languages
                .is_empty()
                .then_some(grant.is_empty())
                .unwrap_or_else(|| {
                    grant.is_empty()
                        || template
                            .allowed_languages
                            .iter()
                            .all(|language| grant.contains(language))
                }),
            (
                ResourceSelector::ProgramTemplate { template: grant },
                ResourceSelector::ProgramTemplate { template: request },
            ) => grant == request,
            (
                ResourceSelector::Memory {
                    tree: grant_tree,
                    path: grant_path,
                },
                ResourceSelector::Memory {
                    tree: request_tree,
                    path: request_path,
                },
            ) => grant_tree == request_tree && pattern_contains(grant_path, request_path),
            (
                ResourceSelector::Schedule { policy: grant },
                ResourceSelector::Schedule { policy: request },
            ) => grant.is_none() || grant == request,
            (grant, request) => grant == request,
        }
    }
}

fn pattern_contains(grant: &str, request: &str) -> bool {
    if grant == request || grant == "**" {
        return true;
    }
    grant.strip_suffix("/**").is_some_and(|prefix| {
        request == prefix
            || request
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EffectSet(pub BTreeSet<CapabilityRequirement>);

impl EffectSet {
    pub fn pure() -> Self {
        Self::default()
    }

    pub fn from_requirement(requirement: CapabilityRequirement) -> Self {
        Self(BTreeSet::from([requirement]))
    }

    pub fn union(&self, other: &Self) -> Self {
        Self(self.0.union(&other.0).cloned().collect())
    }

    pub fn grants(&self, requested: &Self) -> bool {
        requested
            .0
            .iter()
            .all(|request| self.0.iter().any(|grant| grant.covers(request)))
    }

    pub fn is_pure(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for EffectSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_pure() {
            return f.write_str("pure");
        }
        f.write_str("effects{")?;
        for (index, effect) in self.0.iter().enumerate() {
            if index != 0 {
                f.write_str(", ")?;
            }
            write!(f, "{:?}", effect.capability)?;
        }
        f.write_str("}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dot_patterns_are_workspace_relative() {
        let selector = FileSelector::parse("./generated/**").unwrap();
        assert_eq!(selector.root, ResourceRoot::Workspace);
        assert_eq!(selector.pattern, "generated/**");
        assert!(selector.matches("generated/report.md"));
        assert!(selector.matches("generated/nested/report.md"));
        assert!(!selector.matches("src/main.rs"));
    }

    #[test]
    fn rejects_absolute_and_parent_paths() {
        assert_eq!(
            FileSelector::parse("../secret").unwrap_err(),
            SelectorError::ParentTraversal
        );
        assert_eq!(
            FileSelector::parse("/etc/passwd").unwrap_err(),
            SelectorError::AbsolutePath
        );
        assert_eq!(
            FileSelector::parse(r#"reports\secret"#).unwrap_err(),
            SelectorError::InvalidSeparator
        );
    }

    #[test]
    fn host_machine_root_is_explicit_and_never_inferred_from_an_absolute_path() {
        let host = FileSelector::parse("${host-machine}/var/log/**").unwrap();
        let workspace = FileSelector::parse("./var/log/**").unwrap();

        assert_eq!(host.root, ResourceRoot::HostMachine);
        assert_eq!(host.pattern, "var/log/**");
        assert!(!workspace.contains_selector(&host));
        assert!(!host.contains_selector(&workspace));
        assert_eq!(
            FileSelector::parse("/var/log/system.log").unwrap_err(),
            SelectorError::AbsolutePath
        );
    }

    #[test]
    fn capability_grants_attenuate_to_narrower_patterns() {
        let broad =
            CapabilityRequirement::file(FileOperation::Write, FileSelector::parse("./**").unwrap());
        let narrow = CapabilityRequirement::file(
            FileOperation::Write,
            FileSelector::parse("./generated/**").unwrap(),
        );
        assert!(broad.covers(&narrow));
        assert!(!narrow.covers(&broad));
    }

    #[test]
    fn selector_intersection_chooses_narrower_grant() {
        let broad = FileSelector::parse("./**").unwrap();
        let narrow = FileSelector::parse("./generated/**").unwrap();
        assert_eq!(broad.intersection(&narrow).unwrap(), narrow);
    }

    #[test]
    fn glob_matching_does_not_cross_segments_without_double_star() {
        let selector = FileSelector::parse("./src/*.rs").unwrap();
        assert!(selector.matches("src/lib.rs"));
        assert!(!selector.matches("src/nested/lib.rs"));
    }

    #[test]
    fn parameterized_selector_instantiates_refined_path_arguments() {
        let bound = FileSelector::parse("./reports/**").unwrap();
        let template = FileSelectorTemplate {
            root: ResourceRoot::Workspace,
            parts: vec![FileSelectorTemplatePart::Argument {
                index: 0,
                bound: bound.clone(),
            }],
            upper_bound: bound.clone(),
        };
        let arguments = vec![super::super::types::TypedValue::Path {
            selector: bound,
            relative: "reports/weekly.md".into(),
        }];
        assert_eq!(
            template.instantiate(&arguments).unwrap(),
            FileSelector::parse("./reports/weekly.md").unwrap()
        );
    }

    #[test]
    fn parameterized_selector_rejects_argument_outside_bound() {
        let template = FileSelectorTemplate {
            root: ResourceRoot::Workspace,
            parts: vec![FileSelectorTemplatePart::Argument {
                index: 0,
                bound: FileSelector::parse("./reports/**").unwrap(),
            }],
            upper_bound: FileSelector::parse("./reports/**").unwrap(),
        };
        let arguments = vec![super::super::types::TypedValue::Path {
            selector: FileSelector::parse("./**").unwrap(),
            relative: "secrets/key".into(),
        }];
        assert_eq!(
            template.instantiate(&arguments).unwrap_err(),
            SelectorError::TemplateArgumentOutOfBounds(0)
        );
    }

    #[test]
    fn parameterized_network_and_process_selectors_require_typed_bounded_arguments() {
        let network = NetworkSelectorTemplate {
            host_argument: 0,
            port_argument: 1,
            allowed_hosts: vec!["api.example.test".into()],
            allowed_ports: vec![443],
        };
        assert_eq!(
            network
                .instantiate(&[
                    super::super::types::TypedValue::String("api.example.test".into()),
                    super::super::types::TypedValue::Int(443),
                ])
                .unwrap(),
            ("api.example.test".into(), 443)
        );
        assert_eq!(
            network
                .instantiate(&[
                    super::super::types::TypedValue::String("other.example.test".into()),
                    super::super::types::TypedValue::Int(443),
                ])
                .unwrap_err(),
            SelectorError::NetworkTemplateArgumentOutOfBounds(0)
        );

        let process = ProcessSelectorTemplate {
            executable_argument: 0,
            allowed_executables: vec!["git".into()],
        };
        assert_eq!(
            process
                .instantiate(&[super::super::types::TypedValue::String("git".into())])
                .unwrap(),
            "git"
        );
        assert_eq!(
            process
                .instantiate(&[super::super::types::TypedValue::String("sh".into())])
                .unwrap_err(),
            SelectorError::ProcessTemplateArgumentOutOfBounds(0)
        );

        let program = ProgramSelectorTemplate {
            language_argument: 0,
            allowed_languages: vec!["python".into()],
        };
        assert_eq!(
            program
                .instantiate(&[super::super::types::TypedValue::String(" PYTHON ".into())])
                .unwrap(),
            "python"
        );
        assert_eq!(
            program
                .instantiate(&[super::super::types::TypedValue::String("bash".into())])
                .unwrap_err(),
            SelectorError::ProgramTemplateArgumentOutOfBounds(0)
        );
    }

    #[test]
    fn agent_capability_attenuates_provider_and_fork_budgets() {
        let grant = CapabilityRequirement {
            capability: CapabilityKind::AgentSpawn,
            selector: ResourceSelector::Agent {
                providers: vec!["claude".into(), "grok".into()],
                max_depth: 3,
                max_children: 8,
            },
        };
        let narrow = CapabilityRequirement {
            capability: CapabilityKind::AgentSpawn,
            selector: ResourceSelector::Agent {
                providers: vec!["grok".into()],
                max_depth: 2,
                max_children: 4,
            },
        };
        assert!(grant.covers(&narrow));
        assert!(!narrow.covers(&grant));
    }
}
