use rspack_core::{
  Compilation, RuntimeGlobals, RuntimeModule, RuntimeModuleGenerateContext, RuntimeTemplate,
  impl_runtime_module,
};

const RUNTIME_MODULE_VARIABLES: &[&str] = &["deferred"];

#[impl_runtime_module(runtime_module_variables)]
#[derive(Debug)]
pub struct OnChunkLoadedRuntimeModule {}

impl OnChunkLoadedRuntimeModule {
  pub fn new(runtime_template: &RuntimeTemplate) -> Self {
    Self::with_default(runtime_template)
  }
}

#[async_trait::async_trait]
impl RuntimeModule for OnChunkLoadedRuntimeModule {
  fn runtime_module_variables() -> &'static [&'static str] {
    RUNTIME_MODULE_VARIABLES
  }

  fn runtime_requirements(
    &self,
    _compilation: &Compilation,
  ) -> rspack_core::RuntimeModuleRuntimeRequirements {
    rspack_core::RuntimeModuleRuntimeRequirements {
      define: { RuntimeGlobals::ON_CHUNKS_LOADED },
      ..Default::default()
    }
  }

  fn template(&self) -> Vec<(String, String)> {
    vec![(
      self.id().to_string(),
      include_str!("runtime/on_chunk_loaded.ejs").to_string(),
    )]
  }

  async fn generate(
    &self,
    context: &RuntimeModuleGenerateContext<'_>,
  ) -> rspack_error::Result<String> {
    let source = context.runtime_template.render(self.id(), None)?;

    Ok(source)
  }
}
