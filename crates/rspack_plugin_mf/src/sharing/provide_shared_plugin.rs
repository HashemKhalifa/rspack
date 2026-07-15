use std::{
  fmt,
  path::Path,
  sync::{Arc, LazyLock},
};

use regex::Regex;
use rspack_core::{
  BoxDependency, BoxModule, Compilation, CompilationParams, CompilerCompilation,
  CompilerFinishMake, DependencyId, DependencyType, EntryOptions, ModuleFactoryCreateData,
  NormalModuleCreateData, NormalModuleFactoryBeforeResolve, NormalModuleFactoryModule, Plugin,
};
use rspack_error::{Diagnostic, Result};
use rspack_hook::{plugin, plugin_hook};
use rspack_loader_runner::ResourceData;
use rustc_hash::{FxHashMap, FxHashSet};
use tokio::sync::RwLock;

use super::{
  RequestMatchKey, find_ancestor_description_data, find_exact_match, find_prefix_match,
  provide_shared_dependency::ProvideSharedDependency,
  provide_shared_module_factory::ProvideSharedModuleFactory,
};
use crate::{ConsumeVersion, ShareScope};

static RELATIVE_REQUEST: LazyLock<Regex> =
  LazyLock::new(|| Regex::new(r"^(\/|[A-Za-z]:\\|\\\\|\.\.?(\/|$))").expect("Invalid regex"));
static ABSOLUTE_REQUEST: LazyLock<Regex> =
  LazyLock::new(|| Regex::new(r"^(\/|[A-Za-z]:\\|\\\\)").expect("Invalid regex"));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvideOptions {
  #[doc(hidden)]
  pub config_id: usize,
  pub request: Option<String>,
  pub layer: Option<String>,
  pub share_key: String,
  pub share_scope: ShareScope,
  pub version: Option<ProvideVersion>,
  pub eager: bool,
  pub singleton: Option<bool>,
  pub required_version: Option<ConsumeVersion>,
  pub strict_version: Option<bool>,
  pub tree_shaking_mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedProvideOptions {
  config_id: usize,
  dependency_ids: Option<FxHashSet<DependencyId>>,
  pub request: Option<String>,
  pub layer: Option<String>,
  pub share_key: String,
  pub share_scope: ShareScope,
  pub version: ProvideVersion,
  pub eager: bool,
  pub singleton: Option<bool>,
  pub required_version: Option<ConsumeVersion>,
  pub strict_version: Option<bool>,
  pub tree_shaking_mode: Option<String>,
}

#[derive(Clone, Copy)]
struct ResolvedProvideResource<'a> {
  request: &'a str,
  data: &'a ResourceData,
}

impl ProvideOptions {
  fn to_versioned(&self) -> VersionedProvideOptions {
    VersionedProvideOptions {
      config_id: self.config_id,
      dependency_ids: None,
      request: self.request.clone(),
      layer: self.layer.clone(),
      share_key: self.share_key.clone(),
      share_scope: self.share_scope.clone(),
      version: self.version.clone().unwrap_or_default(),
      eager: self.eager,
      singleton: self.singleton,
      required_version: self.required_version.clone(),
      strict_version: self.strict_version,
      tree_shaking_mode: self.tree_shaking_mode.clone(),
    }
  }
}

#[rspack_cacheable::cacheable]
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash)]
pub enum ProvideVersion {
  Version(String),
  #[default]
  False,
}

impl fmt::Display for ProvideVersion {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      ProvideVersion::Version(v) => write!(f, "{v}"),
      ProvideVersion::False => write!(f, "0"),
    }
  }
}

fn insert_unique_config<T: PartialEq>(
  configs: &mut FxHashMap<RequestMatchKey, Vec<T>>,
  key: RequestMatchKey,
  config: T,
) {
  let entries = configs.entry(key).or_default();
  if !entries.contains(&config) {
    entries.push(config);
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ResolvedProvideIdentity {
  lookup_key: RequestMatchKey,
  config_id: usize,
}

#[derive(Debug, Default)]
struct ResolvedProvides {
  configs: FxHashMap<RequestMatchKey, Vec<VersionedProvideOptions>>,
  dependency_index: FxHashMap<DependencyId, FxHashSet<ResolvedProvideIdentity>>,
}

impl ResolvedProvides {
  fn insert(&mut self, key: RequestMatchKey, mut config: VersionedProvideOptions) {
    if config
      .dependency_ids
      .as_ref()
      .is_some_and(FxHashSet::is_empty)
    {
      return;
    }
    let identity = ResolvedProvideIdentity {
      lookup_key: key.clone(),
      config_id: config.config_id,
    };
    if let Some(dependency_ids) = &config.dependency_ids {
      for dependency_id in dependency_ids {
        self
          .dependency_index
          .entry(*dependency_id)
          .or_default()
          .insert(identity.clone());
      }
    }

    let entries = self.configs.entry(key).or_default();
    if let Some(existing) = entries
      .iter_mut()
      .find(|existing| existing.config_id == config.config_id)
    {
      if let Some(existing_dependency_ids) = existing.dependency_ids.take() {
        config
          .dependency_ids
          .get_or_insert_default()
          .extend(existing_dependency_ids);
      }
      *existing = config;
    } else {
      entries.push(config);
    }
  }

  fn remove_dependency(&mut self, dependency_id: &DependencyId) {
    let Some(identities) = self.dependency_index.remove(dependency_id) else {
      return;
    };
    for identity in identities {
      let remove_key = self
        .configs
        .get_mut(&identity.lookup_key)
        .is_some_and(|entries| {
          entries.retain_mut(|config| {
            if config.config_id != identity.config_id {
              return true;
            }
            let Some(dependency_ids) = &mut config.dependency_ids else {
              return true;
            };
            dependency_ids.remove(dependency_id);
            !dependency_ids.is_empty()
          });
          entries.is_empty()
        });
      if remove_key {
        self.configs.remove(&identity.lookup_key);
      }
    }
  }

  fn remove_dependencies(&mut self, dependency_ids: impl IntoIterator<Item = DependencyId>) {
    for dependency_id in dependency_ids {
      self.remove_dependency(&dependency_id);
    }
  }

  fn retain_dependencies(&mut self, mut retain: impl FnMut(&DependencyId) -> bool) {
    let removed = self
      .dependency_index
      .keys()
      .filter(|dependency_id| !retain(dependency_id))
      .copied()
      .collect::<Vec<_>>();
    self.remove_dependencies(removed);
  }
}

fn insert_unique_prefix_config<T: PartialEq>(
  configs: &mut Vec<(RequestMatchKey, Vec<T>)>,
  key: RequestMatchKey,
  config: T,
) {
  if let Some((_, entries)) = configs.iter_mut().find(|(existing, _)| existing == &key) {
    if !entries.contains(&config) {
      entries.push(config);
    }
  } else {
    configs.push((key, vec![config]));
  }
}

fn provide_dependencies(
  configs: &FxHashMap<RequestMatchKey, Vec<VersionedProvideOptions>>,
) -> Vec<ProvideSharedDependency> {
  configs
    .iter()
    .flat_map(|(lookup_key, configs)| {
      configs.iter().map(move |config| {
        ProvideSharedDependency::new(
          config.share_scope.clone(),
          config.share_key.clone(),
          config.version.clone(),
          config
            .request
            .clone()
            .unwrap_or_else(|| lookup_key.request().to_string()),
          config.eager,
          config.singleton,
          config.required_version.clone(),
          config.strict_version,
          config.layer.clone(),
          config.tree_shaking_mode.clone(),
        )
      })
    })
    .collect()
}

#[plugin]
#[derive(Debug)]
pub struct ProvideSharedPlugin {
  provides: Vec<(String, ProvideOptions)>,
  resolved_provides: RwLock<ResolvedProvides>,
  match_provides: RwLock<FxHashMap<RequestMatchKey, Vec<ProvideOptions>>>,
  prefix_match_provides: RwLock<Vec<(RequestMatchKey, Vec<ProvideOptions>)>>,
}

impl ProvideSharedPlugin {
  pub fn new(mut provides: Vec<(String, ProvideOptions)>) -> Self {
    for (config_id, (_, config)) in provides.iter_mut().enumerate() {
      config.config_id = config_id;
    }
    Self::new_inner(
      provides,
      Default::default(),
      Default::default(),
      Default::default(),
    )
  }

  /// For secondary entry points (e.g. `@mui/material/styles`) whose own
  /// `package.json` has no `version`, walk up to the parent package and use
  /// its version — but only when the shared key matches
  /// `<parent_name>/<relative_path>`.
  fn find_parent_package_version(description_path: &Path, share_key: &str) -> Option<String> {
    let entry_dir = if description_path
      .file_name()
      .is_some_and(|name| name == "package.json")
    {
      description_path.parent()?
    } else {
      description_path
    };

    find_ancestor_description_data(entry_dir, |dir, parent| {
      let parent_name = parent.get("name").and_then(|n| n.as_str())?;
      let parent_version = parent.get("version").and_then(|v| v.as_str())?;
      let rel = entry_dir.strip_prefix(dir).ok()?;
      let rel_posix: String = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
      let expected_key = format!("{parent_name}/{rel_posix}");
      (share_key == expected_key).then(|| parent_version.to_string())
    })
  }

  #[allow(clippy::too_many_arguments)]
  pub async fn provide_shared_module(
    &self,
    config_id: usize,
    key: &str,
    share_key: &str,
    share_scope: &ShareScope,
    version: Option<&ProvideVersion>,
    eager: bool,
    singleton: Option<bool>,
    required_version: Option<ConsumeVersion>,
    strict_version: Option<bool>,
    tree_shaking_mode: Option<String>,
    layer: Option<String>,
    resource: &str,
    resource_data: &ResourceData,
    add_diagnostic: impl FnMut(Diagnostic),
  ) {
    let config = ProvideOptions {
      config_id,
      request: None,
      layer,
      share_key: share_key.to_string(),
      share_scope: share_scope.clone(),
      version: version.cloned(),
      eager,
      singleton,
      required_version,
      strict_version,
      tree_shaking_mode,
    };
    self
      .provide_shared_module_from_config(
        &config,
        key,
        share_key,
        ResolvedProvideResource {
          request: resource,
          data: resource_data,
        },
        None,
        add_diagnostic,
      )
      .await;
  }

  async fn provide_shared_module_from_config(
    &self,
    config: &ProvideOptions,
    key: &str,
    share_key: &str,
    resolved: ResolvedProvideResource<'_>,
    dependency_ids: Option<&FxHashSet<DependencyId>>,
    mut add_diagnostic: impl FnMut(Diagnostic),
  ) {
    let ResolvedProvideResource {
      request: resource,
      data: resource_data,
    } = resolved;
    let title = "rspack.ProvideSharedPlugin";
    let error_header = "No version specified and unable to automatically determine one.";
    let lookup_key = RequestMatchKey::new(resource, config.layer.as_deref());
    if let Some(version) = &config.version {
      let mut resolved_provides = self.resolved_provides.write().await;
      resolved_provides.insert(
        lookup_key.clone(),
        VersionedProvideOptions {
          config_id: config.config_id,
          dependency_ids: dependency_ids.cloned(),
          request: Some(resource.to_string()),
          layer: config.layer.clone(),
          share_key: share_key.to_string(),
          share_scope: config.share_scope.clone(),
          version: version.clone(),
          eager: config.eager,
          singleton: config.singleton,
          strict_version: config.strict_version,
          required_version: config.required_version.clone(),
          tree_shaking_mode: config.tree_shaking_mode.clone(),
        },
      );
    } else if let Some(description) = resource_data.description() {
      let version = description
        .json()
        .as_object()
        .and_then(|d| d.get("version"))
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
        .or_else(|| Self::find_parent_package_version(description.path(), share_key));

      if let Some(version) = version {
        let mut resolved_provides = self.resolved_provides.write().await;
        resolved_provides.insert(
          lookup_key.clone(),
          VersionedProvideOptions {
            config_id: config.config_id,
            dependency_ids: dependency_ids.cloned(),
            request: Some(resource.to_string()),
            layer: config.layer.clone(),
            share_key: share_key.to_string(),
            share_scope: config.share_scope.clone(),
            version: ProvideVersion::Version(version),
            eager: config.eager,
            singleton: config.singleton,
            strict_version: config.strict_version,
            required_version: config.required_version.clone(),
            tree_shaking_mode: config.tree_shaking_mode.clone(),
          },
        );
      } else {
        add_diagnostic(Diagnostic::warn(
          title.to_string(),
          format!(
            "{error_header} No version in description file (usually package.json). Add version to description file {}, or manually specify version in shared config. shared module {key} -> {resource}",
            description.path().display()
          ),
        ));
      }
    } else {
      add_diagnostic(Diagnostic::warn(
        title.to_string(),
        format!(
          "{error_header} No description file (usually package.json) found. Add description file with name and version, or manually specify version in shared config. shared module {key} -> {resource}"
        ),
      ));
    }
  }
}

#[plugin_hook(CompilerCompilation for ProvideSharedPlugin)]
async fn compilation(
  &self,
  compilation: &mut Compilation,
  params: &mut CompilationParams,
) -> Result<()> {
  compilation.set_dependency_factory(
    DependencyType::ProvideModuleForShared,
    params.normal_module_factory.clone(),
  );
  compilation.set_dependency_factory(
    DependencyType::ProvideSharedModule,
    Arc::new(ProvideSharedModuleFactory::default()),
  );

  let mut resolved_provides = self.resolved_provides.write().await;
  let mut match_provides = self.match_provides.write().await;
  let mut prefix_match_provides = self.prefix_match_provides.write().await;
  match_provides.clear();
  prefix_match_provides.clear();
  for (request, config) in &self.provides {
    let actual_request = config.request.as_deref().unwrap_or(request);
    let lookup_key = RequestMatchKey::new(actual_request, config.layer.as_deref());
    if RELATIVE_REQUEST.is_match(actual_request) || ABSOLUTE_REQUEST.is_match(actual_request) {
      resolved_provides.insert(lookup_key, config.to_versioned());
    } else if actual_request.ends_with('/') {
      insert_unique_prefix_config(&mut prefix_match_provides, lookup_key, config.clone());
    } else {
      insert_unique_config(&mut match_provides, lookup_key, config.clone());
    }
  }
  Ok(())
}

#[plugin_hook(CompilerFinishMake for ProvideSharedPlugin)]
async fn finish_make(&self, compilation: &mut Compilation) -> Result<()> {
  let entries = {
    let module_graph = compilation.get_module_graph();
    let mut resolved_provides = self.resolved_provides.write().await;
    resolved_provides.retain_dependencies(|dependency_id| {
      module_graph
        .connection_by_dependency_id(dependency_id)
        .is_some()
    });
    provide_dependencies(&resolved_provides.configs)
      .into_iter()
      .map(|dependency| {
        (
          Box::new(dependency) as BoxDependency,
          EntryOptions {
            name: None,
            ..Default::default()
          },
        )
      })
      .collect::<Vec<_>>()
  };
  compilation.add_include(entries).await?;
  Ok(())
}

#[plugin_hook(NormalModuleFactoryModule for ProvideSharedPlugin)]
async fn normal_module_factory_module(
  &self,
  data: &mut ModuleFactoryCreateData,
  create_data: &NormalModuleCreateData,
  module: &mut BoxModule,
) -> Result<()> {
  let dependency_ids = data
    .dependencies
    .iter()
    .map(|dependency| *dependency.id())
    .collect::<FxHashSet<_>>();
  let resource_data = create_data.resource_resolve_data.as_ref();
  let resolved = ResolvedProvideResource {
    request: resource_data.resource(),
    data: resource_data,
  };
  let effective_layer = module
    .get_layer()
    .cloned()
    .or_else(|| data.issuer_layer.clone());
  let request = &data.request;
  let matched = {
    let match_provides = self.match_provides.read().await;
    find_exact_match(&match_provides, request, effective_layer.as_deref()).cloned()
  };
  if let Some(configs) = matched {
    for config in configs {
      self
        .provide_shared_module_from_config(
          &config,
          request,
          &config.share_key,
          resolved,
          Some(&dependency_ids),
          |d| data.diagnostics.push(d),
        )
        .await;
    }
  }
  let prefix_match = {
    let prefix_match_provides = self.prefix_match_provides.read().await;
    find_prefix_match(&prefix_match_provides, request, effective_layer.as_deref())
      .map(|(config, remainder)| (config.clone(), remainder.to_string()))
  };
  if let Some((configs, remainder)) = prefix_match {
    for config in configs {
      self
        .provide_shared_module_from_config(
          &config,
          request,
          &(config.share_key.clone() + remainder.as_str()),
          resolved,
          Some(&dependency_ids),
          |d| data.diagnostics.push(d),
        )
        .await;
    }
  }
  Ok(())
}

#[plugin_hook(NormalModuleFactoryBeforeResolve for ProvideSharedPlugin)]
async fn normal_module_factory_before_resolve(
  &self,
  data: &mut ModuleFactoryCreateData,
) -> Result<Option<bool>> {
  let dependency_ids = data
    .dependencies
    .iter()
    .map(|dependency| *dependency.id())
    .collect::<Vec<_>>();
  let has_resolved_dependency = {
    let resolved_provides = self.resolved_provides.read().await;
    dependency_ids.iter().any(|dependency_id| {
      resolved_provides
        .dependency_index
        .contains_key(dependency_id)
    })
  };
  if !has_resolved_dependency {
    return Ok(None);
  }
  let mut resolved_provides = self.resolved_provides.write().await;
  resolved_provides.remove_dependencies(dependency_ids);
  Ok(None)
}

impl Plugin for ProvideSharedPlugin {
  fn name(&self) -> &'static str {
    "rspack.ProvideSharedPlugin"
  }

  fn apply(&self, ctx: &mut rspack_core::ApplyContext<'_>) -> Result<()> {
    ctx.compiler_hooks.compilation.tap(compilation::new(self));
    ctx.compiler_hooks.finish_make.tap(finish_make::new(self));
    ctx
      .normal_module_factory_hooks
      .before_resolve
      .tap(normal_module_factory_before_resolve::new(self));
    ctx
      .normal_module_factory_hooks
      .module
      .tap(normal_module_factory_module::new(self));
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use rspack_core::DependencyId;
  use rustc_hash::FxHashMap;

  use super::{
    ProvideOptions, ProvideVersion, ResolvedProvides, insert_unique_config,
    insert_unique_prefix_config,
  };
  use crate::{
    ShareScope,
    sharing::{RequestMatchKey, find_exact_match, find_prefix_match},
  };

  fn provide_options(share_key: &str, share_scope: &str, layer: Option<&str>) -> ProvideOptions {
    ProvideOptions {
      config_id: 0,
      request: Some("pkg".to_string()),
      layer: layer.map(str::to_string),
      share_key: share_key.to_string(),
      share_scope: ShareScope::Single(share_scope.to_string()),
      version: Some(ProvideVersion::Version("1.0.0".to_string())),
      eager: false,
      singleton: None,
      required_version: None,
      strict_version: None,
      tree_shaking_mode: None,
    }
  }

  #[test]
  fn same_request_and_layer_keep_every_provider_identity() {
    let key = RequestMatchKey::new("pkg", Some("server"));
    let mut matches = FxHashMap::default();
    insert_unique_config(
      &mut matches,
      key.clone(),
      provide_options("pkg-a", "scope-a", Some("server")),
    );
    insert_unique_config(
      &mut matches,
      key,
      provide_options("pkg-b", "scope-b", Some("server")),
    );

    let matched = find_exact_match(&matches, "pkg", Some("server")).expect("match");
    assert_eq!(matched.len(), 2);

    let resolved = matched
      .iter()
      .cloned()
      .enumerate()
      .map(|(config_id, mut options)| {
        options.config_id = config_id;
        options.to_versioned()
      })
      .fold(ResolvedProvides::default(), |mut resolved, options| {
        resolved.insert(
          RequestMatchKey::new("/resolved/pkg.js", Some("server")),
          options,
        );
        resolved
      });
    let resolved = resolved
      .configs
      .get(&RequestMatchKey::new("/resolved/pkg.js", Some("server")))
      .expect("resolved providers");
    assert_eq!(resolved.len(), 2);
    assert!(resolved.iter().any(|options| {
      options.share_key == "pkg-a"
        && options.share_scope == ShareScope::Single("scope-a".to_string())
    }));
    assert!(resolved.iter().any(|options| {
      options.share_key == "pkg-b"
        && options.share_scope == ShareScope::Single("scope-b".to_string())
    }));
  }

  #[test]
  fn resolved_provider_version_is_replaced_without_dropping_other_providers() {
    let key = RequestMatchKey::new("/resolved/pkg.js", Some("server"));
    let mut resolved = ResolvedProvides::default();
    let first = provide_options("pkg-a", "scope-a", Some("server")).to_versioned();
    let mut other_options = provide_options("pkg-b", "scope-b", Some("server"));
    other_options.config_id = 1;
    let other = other_options.to_versioned();
    resolved.insert(key.clone(), first.clone());
    resolved.insert(key.clone(), other.clone());

    let mut updated = first;
    updated.version = ProvideVersion::Version("2.0.0".to_string());
    resolved.insert(key.clone(), updated);

    let providers = resolved.configs.get(&key).expect("resolved providers");
    assert_eq!(providers.len(), 2);
    assert!(providers.iter().any(|provider| {
      provider.share_key == "pkg-a"
        && provider.version == ProvideVersion::Version("2.0.0".to_string())
    }));
    assert!(providers.iter().any(|provider| provider == &other));
  }

  #[test]
  fn configured_versions_of_the_same_shared_identity_coexist() {
    let key = RequestMatchKey::new("/resolved/pkg.js", None);
    let mut resolved = ResolvedProvides::default();
    let first = provide_options("pkg", "default", None).to_versioned();
    let mut second_options = provide_options("pkg", "default", None);
    second_options.config_id = 1;
    second_options.version = Some(ProvideVersion::Version("2.0.0".to_string()));
    let second = second_options.to_versioned();

    resolved.insert(key.clone(), first);
    resolved.insert(key.clone(), second);

    let providers = resolved.configs.get(&key).expect("resolved providers");
    assert_eq!(providers.len(), 2);
    assert!(
      providers
        .iter()
        .any(|provider| provider.version == ProvideVersion::Version("1.0.0".to_string()))
    );
    assert!(
      providers
        .iter()
        .any(|provider| provider.version == ProvideVersion::Version("2.0.0".to_string()))
    );
  }

  #[test]
  fn dependency_resolutions_move_without_dropping_other_issuers() {
    let old_key = RequestMatchKey::new("/old/pkg.js", None);
    let new_key = RequestMatchKey::new("/new/pkg.js", None);
    let moved_dependency = DependencyId::from(1);
    let unchanged_dependency = DependencyId::from(2);
    let mut resolved = ResolvedProvides::default();

    let mut moved_from_old = provide_options("pkg", "default", None).to_versioned();
    moved_from_old.dependency_ids = Some([moved_dependency].into_iter().collect());
    resolved.insert(old_key.clone(), moved_from_old);
    let mut unchanged_at_old = provide_options("pkg", "default", None).to_versioned();
    unchanged_at_old.dependency_ids = Some([unchanged_dependency].into_iter().collect());
    resolved.insert(old_key.clone(), unchanged_at_old);

    assert_eq!(
      resolved.configs[&old_key][0]
        .dependency_ids
        .as_ref()
        .expect("dynamic provider dependencies")
        .len(),
      2
    );

    resolved.remove_dependency(&moved_dependency);
    let old_dependencies = resolved.configs[&old_key][0]
      .dependency_ids
      .as_ref()
      .expect("dynamic provider dependencies");
    assert_eq!(old_dependencies.len(), 1);
    assert!(old_dependencies.contains(&unchanged_dependency));

    let mut moved = provide_options("pkg", "default", None).to_versioned();
    moved.dependency_ids = Some([moved_dependency].into_iter().collect());
    resolved.insert(new_key.clone(), moved);

    assert_eq!(resolved.configs[&old_key].len(), 1);
    assert_eq!(resolved.configs[&new_key].len(), 1);

    resolved.retain_dependencies(|dependency_id| dependency_id == &moved_dependency);
    assert!(!resolved.configs.contains_key(&old_key));
    assert_eq!(resolved.configs[&new_key].len(), 1);
  }

  #[test]
  fn exact_layer_precedes_fallback_and_prefix_order_stays_deterministic() {
    let mut matches = FxHashMap::default();
    insert_unique_config(
      &mut matches,
      RequestMatchKey::new("pkg", None),
      provide_options("fallback", "default", None),
    );
    insert_unique_config(
      &mut matches,
      RequestMatchKey::new("pkg", Some("server")),
      provide_options("exact", "server", Some("server")),
    );
    assert_eq!(
      find_exact_match(&matches, "pkg", Some("server")).expect("exact match")[0].share_key,
      "exact"
    );
    assert_eq!(
      find_exact_match(&matches, "pkg", Some("client")).expect("fallback match")[0].share_key,
      "fallback"
    );

    let mut prefixes = Vec::new();
    insert_unique_prefix_config(
      &mut prefixes,
      RequestMatchKey::new("pkg/", Some("server")),
      provide_options("short", "server", Some("server")),
    );
    insert_unique_prefix_config(
      &mut prefixes,
      RequestMatchKey::new("pkg/feature/", None),
      provide_options("long", "default", None),
    );
    let (matched, remainder) =
      find_prefix_match(&prefixes, "pkg/feature/button", Some("server")).expect("prefix match");
    assert_eq!(matched[0].share_key, "long");
    assert_eq!(remainder, "button");
  }
}
