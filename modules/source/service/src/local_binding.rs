use std::sync::Arc;
use async_trait::async_trait;
use mintcat_game_api::GameDescriptor;
use mintcat_runtime_broker::CancellationToken;
use mintcat_runtime_downloads::DownloadCatalog;
use mintcat_sdk::ModRef;
use mintcat_source_api::{DownloadDescriptor, SearchPage, SearchRequest, SourceDependency, SourceError, SourceGameBinding, SourceMod, SourceProvider, SourceVersion};
use url::Url;

/// The local input source is not a remote catalog. Its only authority comes
/// from the selected-input broker; remote metadata cannot create local bytes.
pub struct LocalInputProvider;
impl LocalInputProvider {
    pub fn register(catalog:&DownloadCatalog, descriptor:&GameDescriptor,module_lock_digest:&str)->Result<(),SourceError> {
        descriptor.validate().map_err(|error|SourceError::InvalidArgument(error.to_string()))?;
        let binding=descriptor.source_bindings.iter().find(|binding|binding.source_id=="local").ok_or_else(||SourceError::InvalidArgument("signed game descriptor has no local input binding".into()))?;
        let existing = catalog.provider_module_lock("local").map_err(|error|SourceError::InvalidArgument(error.to_string()))?;
        if existing.as_deref().is_some_and(|lock|lock != module_lock_digest) { return Err(SourceError::InvalidArgument("local input provider belongs to another module lock".into())); }
        if existing.is_none() {
            catalog.register_provider(Arc::new(Self),module_lock_digest).map_err(|error|SourceError::InvalidArgument(error.to_string()))?;
        }
        catalog.register_game("local",SourceGameBinding {game_id:descriptor.game_id.clone(),remote_game_key:binding.remote_game_key.clone(),provider_game_id:binding.provider_game_id.clone()}).map_err(|error|SourceError::InvalidArgument(error.to_string()))
    }
}
fn input_required()->SourceError {SourceError::Unavailable("local source requires an explicit file or directory selection grant through mods.importLocal".into())}
#[async_trait]
impl SourceProvider for LocalInputProvider {
    fn source_id(&self)->&'static str {"local"}
    fn parse_url(&self,_game:&SourceGameBinding,_url:&Url)->Result<Option<ModRef>,SourceError> {Err(input_required())}
    async fn search(&self,_game:&SourceGameBinding,_request:&SearchRequest,_cancellation:&CancellationToken)->Result<SearchPage,SourceError> {Err(input_required())}
    async fn get_mod(&self,_game:&SourceGameBinding,_reference:&ModRef,_cancellation:&CancellationToken)->Result<SourceMod,SourceError> {Err(input_required())}
    async fn list_versions(&self,_game:&SourceGameBinding,_reference:&ModRef,_cancellation:&CancellationToken)->Result<Vec<SourceVersion>,SourceError> {Err(input_required())}
    async fn get_dependencies(&self,_game:&SourceGameBinding,_reference:&ModRef,_cancellation:&CancellationToken)->Result<Vec<SourceDependency>,SourceError> {Err(input_required())}
    async fn resolve_download(&self,_game:&SourceGameBinding,_reference:&ModRef,_cancellation:&CancellationToken)->Result<DownloadDescriptor,SourceError> {Err(input_required())}
}
