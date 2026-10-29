//! Exact EffectCraft Photo Filter palette; labels never act as serialized IDs.
//! MIT selected from MIT OR Apache-2.0; see craft-photo-filter provenance.
use serde::{Serialize,Deserialize};
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Hash,Serialize,Deserialize)]
#[repr(u8)]
pub enum PhotoFilterPreset{
 #[default]
 #[serde(rename="warming_85")] Warming85=0,
 #[serde(rename="warming_lba")] WarmingLba=1,
 #[serde(rename="warming_81")] Warming81=2,
 #[serde(rename="cooling_80")] Cooling80=3,
 #[serde(rename="cooling_lbb")] CoolingLbb=4,
 #[serde(rename="cooling_82")] Cooling82=5,
 #[serde(rename="red")] Red=6,
 #[serde(rename="orange")] Orange=7,
 #[serde(rename="yellow")] Yellow=8,
 #[serde(rename="green")] Green=9,
 #[serde(rename="cyan")] Cyan=10,
 #[serde(rename="blue")] Blue=11,
 #[serde(rename="violet")] Violet=12,
 #[serde(rename="magenta")] Magenta=13,
 #[serde(rename="sepia")] Sepia=14,
 #[serde(rename="deep_red")] DeepRed=15,
 #[serde(rename="deep_blue")] DeepBlue=16,
 #[serde(rename="deep_emerald")] DeepEmerald=17,
 #[serde(rename="deep_yellow")] DeepYellow=18,
 #[serde(rename="underwater")] Underwater=19,
 #[serde(rename="custom")] Custom=20,
}
impl PhotoFilterPreset{
 pub const ALL:[Self;21]=[Self::Warming85,Self::WarmingLba,Self::Warming81,Self::Cooling80,Self::CoolingLbb,Self::Cooling82,Self::Red,Self::Orange,Self::Yellow,Self::Green,Self::Cyan,Self::Blue,Self::Violet,Self::Magenta,Self::Sepia,Self::DeepRed,Self::DeepBlue,Self::DeepEmerald,Self::DeepYellow,Self::Underwater,Self::Custom];
 pub const fn label(self)->&'static str{match self{
  Self::Warming85=>"Warming Filter (85)",
  Self::WarmingLba=>"Warming Filter (LBA)",
  Self::Warming81=>"Warming Filter (81)",
  Self::Cooling80=>"Cooling Filter (80)",
  Self::CoolingLbb=>"Cooling Filter (LBB)",
  Self::Cooling82=>"Cooling Filter (82)",
  Self::Red=>"Red",
  Self::Orange=>"Orange",
  Self::Yellow=>"Yellow",
  Self::Green=>"Green",
  Self::Cyan=>"Cyan",
  Self::Blue=>"Blue",
  Self::Violet=>"Violet",
  Self::Magenta=>"Magenta",
  Self::Sepia=>"Sepia",
  Self::DeepRed=>"Deep Red",
  Self::DeepBlue=>"Deep Blue",
  Self::DeepEmerald=>"Deep Emerald",
  Self::DeepYellow=>"Deep Yellow",
  Self::Underwater=>"Underwater",
  Self::Custom=>"Custom",
 }}
 /// None means use the actual separately authored Custom float color.
 pub const fn rgb8(self)->Option<[u8;3]>{match self{
  Self::Warming85=>Some([240,140,30]),
  Self::WarmingLba=>Some([245,155,25]),
  Self::Warming81=>Some([240,180,40]),
  Self::Cooling80=>Some([20,110,250]),
  Self::CoolingLbb=>Some([20,95,245]),
  Self::Cooling82=>Some([30,180,250]),
  Self::Red=>Some([230,30,30]),
  Self::Orange=>Some([240,130,30]),
  Self::Yellow=>Some([245,225,35]),
  Self::Green=>Some([30,200,30]),
  Self::Cyan=>Some([30,200,230]),
  Self::Blue=>Some([30,55,230]),
  Self::Violet=>Some([150,30,230]),
  Self::Magenta=>Some([225,30,225]),
  Self::Sepia=>Some([170,120,55]),
  Self::DeepRed=>Some([250,0,0]),
  Self::DeepBlue=>Some([0,40,200]),
  Self::DeepEmerald=>Some([0,135,0]),
  Self::DeepYellow=>Some([255,210,0]),
  Self::Underwater=>Some([0,190,175]),
  Self::Custom=>None,
 }}
 pub fn resolve_color(self,custom:[f32;3])->[f32;3]{self.rgb8().map_or(custom,|color|color.map(|value|f32::from(value)/255.0))}
}
