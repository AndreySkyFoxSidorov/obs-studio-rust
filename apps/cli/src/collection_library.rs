//! Explicit headless adapter over the same managed native-file service.
use super::*;
use skylive_host::collection_library::{self as library,EntryId,Root,View,RemovePlan,Opened};
#[derive(Default)]pub(super) struct State{pub active:Option<PathBuf>}
#[derive(Deserialize)]#[serde(tag="operation",rename_all="snake_case",deny_unknown_fields)]pub(super) enum Action{
 List,New{name:String,#[serde(default)]discard:bool},Duplicate{name:String,#[serde(default)]discard:bool},Open{id:EntryId,#[serde(default)]discard:bool},Remove{#[serde(default)]discard:bool},
}
impl Action{pub fn changes_document(&self)->bool{!matches!(self,Self::List)}fn discards(&self)->bool{match self{Self::List=>false,Self::New{discard,..}|Self::Duplicate{discard,..}|Self::Open{discard,..}|Self::Remove{discard}=>*discard}}}
pub(super) enum Prepared{Listed(View),Replace{opened:Opened,remove:Option<RemovePlan>,created:bool}}
impl From<library::Error> for CliError{fn from(error:library::Error)->Self{Self{code:error.code(),detail:error.to_string(),phase:Some(match error.phase{storage::Phase::BeforePublication=>"before_publication",storage::Phase::AfterPublication=>"after_publication"})}}}
pub(super) fn root()->Result<Root,CliError>{let preferences=skylive_host::user_preferences::default_path().map_err(|error|CliError::from(error.to_string()))?;Root::for_preferences(&preferences).map_err(CliError::from)}
pub(super) fn prepare(owner:&Session,state:&State,action:Action)->Result<Prepared,CliError>{if action.changes_document()&&owner.is_document_dirty()&&!action.discards(){return Err("save dirty document first, or explicitly set discard:true; no managed artifact was created".into());}let view=library::list(&root()?)?;match action{
 Action::List=>Ok(Prepared::Listed(view)),
 Action::New{name,..}=>{let plan=library::CreatePlan::new(owner,&view,name,false,state.active.as_deref())?;Ok(Prepared::Replace{opened:library::create(plan)?,remove:None,created:true})},
 Action::Duplicate{name,..}=>{let plan=library::CreatePlan::new(owner,&view,name,true,state.active.as_deref())?;Ok(Prepared::Replace{opened:library::create(plan)?,remove:None,created:true})},
 Action::Open{id,..}=>Ok(Prepared::Replace{opened:library::open(&view,id)?,remove:None,created:false}),
 Action::Remove{..}=>{let active=state.active.as_deref().ok_or("no accepted native document is currently bound; external/unsaved documents cannot be removed")?;let prepared=library::prepare_remove(&view,active)?;Ok(Prepared::Replace{opened:prepared.alternative,remove:Some(prepared.removal),created:false})},
}}
pub(super) fn remember(path:PathBuf)->Result<(),String>{let preferences=skylive_host::user_preferences::default_path().map_err(|error|error.to_string())?;let marker=skylive_host::last_native_document::metadata_path(&preferences).map_err(|error|error.to_string())?;let last=skylive_host::last_native_document::LastNativeDocument::new(path).map_err(|error|error.to_string())?;skylive_host::last_native_document::save(&marker,&last).map_err(|error|error.to_string())}
