//! One I/O-owned bootstrap, with independent phase results. No native/device opens.
use std::{path::PathBuf,sync::Arc};
use skylive_core::UserPreferences;
use skylive_host::{last_native_document::{self,LastNativeDocument},localization::Localization,storage::{self,LoadedDocument}};
pub struct Native{pub loaded:Box<LoadedDocument>,pub path:PathBuf}
pub struct Loaded{
 pub preferences_path:Option<PathBuf>,pub preferences:Result<Option<UserPreferences>,String>,
 pub localization_path:PathBuf,pub localization:Arc<Localization>,pub localization_error:Option<String>,
 pub metadata_path:Option<PathBuf>,pub remembered:Result<Option<LastNativeDocument>,String>,pub native:Option<Result<Native,String>>,
}
/// Invoked ONLY by the existing IoWorker. Even localization path selection can
/// stat candidate files, so external_path belongs here, not CreationContext.
fn metadata()->Result<Loaded,String>{
 let preferences_path=skylive_host::user_preferences::default_path();
 let preferences=match &preferences_path{Ok(path)=>skylive_host::user_preferences::load(path).map_err(|error|error.to_string()),Err(error)=>Err(error.to_string())};
 let language=preferences.as_ref().ok().and_then(Option::as_ref).map(|value|value.language()).unwrap_or("en-US");
 let localization_path=crate::i18n::external_path();
 let(localization,localization_error)=match Localization::load(&localization_path,language){Ok(value)=>(value,None),Err(error)=>(Localization::embedded(language).map_err(|error|error.to_string())?,Some(error.to_string()))};
 let metadata_path=preferences_path.as_ref().ok().map(|path|last_native_document::metadata_path(path)).transpose().map_err(|error|error.to_string())?;
 let remembered=metadata_path.as_ref().map_or(Ok(None),|path|last_native_document::load(path).map_err(|error|error.to_string()));
 let native=None;
 Ok(Loaded{preferences_path:preferences_path.ok(),preferences,localization_path,localization:Arc::new(localization),localization_error,metadata_path,remembered,native})
}

/// Strict legacy bootstrap port remains available to existing callers.
pub fn load()->Result<Loaded,String>{let mut loaded=metadata()?;loaded.native=loaded.remembered.as_ref().ok().and_then(Option::as_ref).map(|last|std::fs::canonicalize(last.path()).map_err(|error|format!("{}: {error}",last.path().display())).and_then(|path|{let value=storage::load_with_report(&path).map_err(|error|format!("{}: {error}",path.display()))?;Ok(Native{loaded:Box::new(value),path})}));Ok(loaded)}
/// Production IO actor keeps an unpublished model behind its opaque repair key.
pub fn load_with_repair(owner:&mut skylive_host::document_repair::Owner)->Result<(Loaded,Option<Result<skylive_host::document_repair::Event,String>>),String>{let loaded=metadata()?;let native=loaded.remembered.as_ref().ok().and_then(Option::as_ref).map(|last|owner.begin(skylive_host::document_repair::Begin::Native{path:last.path().to_owned(),recover:false}).map_err(|error|error.to_string()));Ok((loaded,native))}
