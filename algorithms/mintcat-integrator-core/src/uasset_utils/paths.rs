//! Pak 路径与 Game 路径转换（内联自 trumank/uasset_utils，无外部依赖）

pub use typed_path::Utf8Component as PakPathComponentTrait;
pub use typed_path::Utf8UnixComponent as PakPathComponent;
pub use typed_path::Utf8UnixPath as PakPath;
pub use typed_path::Utf8UnixPathBuf as PakPathBuf;

use unicase::eq_ascii;

pub fn pak_path_to_game_path<P: AsRef<PakPath>>(pak_path: P) -> Option<String> {
    let mut components = pak_path.as_ref().components();
    match components.next() {
        Some(PakPathComponent::Normal(c)) if eq_ascii(c, "Engine") => match components.next() {
            Some(PakPathComponent::Normal(c)) if eq_ascii(c, "Content") => {
                Some(PakPath::new("/Engine").join(components.as_path()))
            }
            Some(PakPathComponent::Normal(c)) if eq_ascii(c, "Plugins") => {
                let mut last = None;
                loop {
                    match components.next() {
                        Some(PakPathComponent::Normal(c)) if eq_ascii(c, "Content") => {
                            break last.map(|plugin| {
                                PakPath::new("/").join(plugin).join(components.as_path())
                            })
                        }
                        Some(PakPathComponent::Normal(next)) => {
                            last = Some(next);
                        }
                        _ => break None,
                    }
                }
            }
            _ => None,
        },
        Some(PakPathComponent::Normal(_)) => match components.next() {
            Some(PakPathComponent::Normal(c)) if eq_ascii(c, "Content") => {
                Some(PakPath::new("/Game").join(components))
            }
            _ => None,
        },
        _ => None,
    }
    .map(|p| p.to_string())
}
