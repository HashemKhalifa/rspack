use std::sync::LazyLock;

use rspack_core::{
  Compilation, RuntimeGlobals, RuntimeModule, RuntimeModuleGenerateContext,
  RuntimeModuleRuntimeRequirements, RuntimeTemplate, impl_runtime_module,
  runtime_mode::RuntimeMode,
};
use rspack_plugin_runtime::extract_runtime_globals_from_ejs;
use rspack_util::test::is_hot_test;

static HOT_MODULE_REPLACEMENT_TEMPLATE: &str = include_str!("runtime/hot_module_replacement.ejs");
static HOT_MODULE_REPLACEMENT_RUNTIME_REQUIREMENTS: LazyLock<RuntimeModuleRuntimeRequirements> =
  LazyLock::new(|| {
    let mut requirements = extract_runtime_globals_from_ejs(HOT_MODULE_REPLACEMENT_TEMPLATE);
    requirements
      .define
      .insert(RuntimeGlobals::INTERCEPT_MODULE_EXECUTION);
    requirements
  });
const RUNTIME_MODULE_VARIABLES: &[&str] = &[
  "currentModuleData",
  "hmrInstalledModules",
  "currentChildModule",
  "currentParents",
  "registeredStatusHandlers",
  "currentStatus",
  "blockingPromises",
  "blockingPromisesWaiting",
  "currentUpdateApplyHandlers",
  "queuedInvalidatedModules",
  "createRequire",
  "createModuleHotObject",
  "setStatus",
  "unblock",
  "trackBlockingPromise",
  "waitForBlockingPromises",
  "hotCheck",
  "hotApply",
  "internalApply",
  "applyInvalidatedModules",
];

#[impl_runtime_module]
#[derive(Debug)]
pub struct HotModuleReplacementRuntimeModule {}

impl HotModuleReplacementRuntimeModule {
  pub fn new(runtime_template: &RuntimeTemplate) -> Self {
    Self::with_default(runtime_template)
  }
}

#[async_trait::async_trait]
impl RuntimeModule for HotModuleReplacementRuntimeModule {
  fn runtime_module_variables() -> &'static [&'static str] {
    RUNTIME_MODULE_VARIABLES
  }

  fn template(&self) -> Vec<(String, String)> {
    vec![(
      self.id().to_string(),
      HOT_MODULE_REPLACEMENT_TEMPLATE.to_string(),
    )]
  }

  async fn generate(
    &self,
    context: &RuntimeModuleGenerateContext<'_>,
  ) -> rspack_error::Result<String> {
    let content = context.runtime_template.render(
      self.id().as_str(),
      Some(serde_json::json!({
        "_is_hot_test": is_hot_test(),
        "_is_rspack_runtime_mode": context.compilation.options.experiments.runtime_mode == RuntimeMode::Rspack,
      })),
    )?;

    Ok(content)
  }
  fn runtime_requirements(
    &self,
    _compilation: &Compilation,
  ) -> rspack_core::RuntimeModuleRuntimeRequirements {
    *HOT_MODULE_REPLACEMENT_RUNTIME_REQUIREMENTS
  }
}
