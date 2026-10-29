//! Historical OBS32.2.2 fixed ingest addresses. Pure immutable data, not service
//! authentication, dynamic discovery, service recommendations or connectivity.
use serde::Serialize;
mod catalog;
pub const OBS_VERSION:&str="32.2.2";
pub const SOURCE_COMMIT:&str="ba2f32bdf791005443988a4955e963663e16b1ed";
pub const MAX_QUERY_BYTES:usize=256;
#[derive(Debug,Serialize)]
pub struct ServerPreset {
    pub name:&'static str,
    /// Exact historical URL, including path casing, port and query text.
    pub url:&'static str,
    #[serde(skip)] pub(crate) search:&'static str,
}
#[derive(Debug,Serialize)]
pub struct ServicePreset {
    pub name:&'static str,
    pub common:bool,
    pub aliases:&'static [&'static str],
    pub servers:&'static [ServerPreset],
    #[serde(skip)] pub(crate) search:&'static str,
}
pub use catalog::DATA_SHA256;
pub fn services()->&'static [ServicePreset]{catalog::SERVICES}
pub fn fold_query(query:&str)->Result<String,crate::Error>{
    if query.len()>MAX_QUERY_BYTES||query.chars().any(char::is_control){return Err(crate::Error::Invalid("service preset search requires at most256UTF8bytes without controls"));}
    Ok(query.trim().to_lowercase())
}
impl ServerPreset {pub fn matches_folded(&self,query:&str)->bool{self.search.contains(query)}}
impl ServicePreset {
    pub fn matches_folded(&self,query:&str)->bool{self.search.contains(query)||self.servers.iter().any(|server|server.matches_folded(query))}
}
/// Labels are view metadata. Only this exact existing URL is persisted; aliases,
/// equivalent hosts, removed ports and normalized queries are never inferred.
pub fn find_url(url:&str)->Option<(&'static ServicePreset,&'static ServerPreset)>{
    services().iter().find_map(|service|service.servers.iter().find(|server|server.url==url).map(|server|(service,server)))
}
