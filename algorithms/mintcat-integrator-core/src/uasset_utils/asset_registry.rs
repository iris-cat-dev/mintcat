//! AssetRegistry 读写（内联自 trumank/uasset_utils，UE 5.6 兼容：UTF-16 宽松解码）

use std::io::{Read, Seek, Write};

use anyhow::{anyhow, Context, Result};
use byteorder::{ReadBytesExt, WriteBytesExt, BE, LE};
use unreal_asset::{exports::ExportBaseTrait, types::PackageIndex};
use unreal_asset::{flags::EObjectFlags, reader::ArchiveTrait};

use super::paths::pak_path_to_game_path;

/// UE 5.6 等可能含 lone surrogate 的 UTF-16，宽松解码为 String（无效 surrogate 替换为 U+FFFD）
fn utf16_lenient(s: &[u16]) -> String {
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let u = s[i];
        if u == 0 {
            break;
        }
        if (0xD800..=0xDBFF).contains(&u) {
            let high = u;
            i += 1;
            if i < s.len() {
                let low = s[i];
                if (0xDC00..=0xDFFF).contains(&low) {
                    out.push(
                        char::from_u32(
                            ((high as u32 - 0xD800) << 10 | (low as u32 - 0xDC00)) + 0x1_0000,
                        )
                        .unwrap_or('\u{FFFD}'),
                    );
                    i += 1;
                    continue;
                }
            }
            out.push('\u{FFFD}');
            continue;
        }
        if (0xDC00..=0xDFFF).contains(&u) {
            out.push('\u{FFFD}');
            i += 1;
            continue;
        }
        out.push(char::from_u32(u as u32).unwrap_or('\u{FFFD}'));
        i += 1;
    }
    out
}

pub trait Readable<R> {
    fn read(reader: &mut R) -> Result<Self>
    where
        Self: Sized;
}
pub trait Writable<W> {
    fn write(&self, writer: &mut W) -> Result<()>;
}

type Guid = [u8; 16];
impl<R: Read> Readable<R> for Guid {
    fn read(reader: &mut R) -> Result<Self> {
        let mut buf = [0; 16];
        reader.read_exact(&mut buf)?;
        Ok(buf)
    }
}
impl<W: Write> Writable<W> for Guid {
    fn write(&self, writer: &mut W) -> Result<()> {
        writer.write_all(&self[..])?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NameIndex(u32);
impl<R: Read> Readable<R> for NameIndex {
    fn read(reader: &mut R) -> Result<Self> {
        Ok(NameIndex(reader.read_u32::<LE>()?))
    }
}
impl<W: Write> Writable<W> for NameIndex {
    fn write(&self, writer: &mut W) -> Result<()> {
        writer.write_u32::<LE>(self.0)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NameIndexFlagged(pub u32, pub Option<u32>);
impl<R: Read> Readable<R> for NameIndexFlagged {
    fn read(reader: &mut R) -> Result<Self> {
        let n = reader.read_u32::<LE>()?;
        Ok(if n & 0x80000000 != 0 {
            let flag = Some(reader.read_u32::<LE>()?);
            NameIndexFlagged(n << 1 >> 1, flag)
        } else {
            NameIndexFlagged(n, None)
        })
    }
}
impl<W: Write> Writable<W> for NameIndexFlagged {
    fn write(&self, writer: &mut W) -> Result<()> {
        if let Some(flag) = self.1 {
            writer.write_u32::<LE>(self.0 | 0x80000000)?;
            writer.write_u32::<LE>(flag)?;
        } else {
            writer.write_u32::<LE>(self.0)?;
        }
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub struct ExportPath {
    pub object_path: NameIndexFlagged,
    pub package_path: NameIndexFlagged,
    pub asset_class: NameIndexFlagged,
}
impl<R: Read> Readable<R> for ExportPath {
    fn read(reader: &mut R) -> Result<Self> {
        Ok(ExportPath {
            object_path: NameIndexFlagged::read(reader)?,
            package_path: NameIndexFlagged::read(reader)?,
            asset_class: NameIndexFlagged::read(reader)?,
        })
    }
}
impl<W: Write> Writable<W> for ExportPath {
    fn write(&self, writer: &mut W) -> Result<()> {
        self.object_path.write(writer)?;
        self.package_path.write(writer)?;
        self.asset_class.write(writer)?;
        Ok(())
    }
}

fn read_array<R, T, E, F>(length: u32, reader: &mut R, mut f: F) -> Result<Vec<T>, E>
where
    F: FnMut(&mut R) -> Result<T, E>,
{
    (0..length).map(|_| f(reader)).collect()
}
fn write_array<W, T, F>(writer: &mut W, array: impl IntoIterator<Item = T>, mut f: F) -> Result<()>
where
    F: FnMut(&mut W, T) -> Result<()>,
{
    for item in array {
        f(writer, item)?;
    }
    Ok(())
}

#[derive(Debug, PartialEq)]
pub struct Pair {
    pub name: NameIndex,
    pub type_: Type,
    pub index: u32,
}

impl<R: Read> Readable<R> for Pair {
    fn read(reader: &mut R) -> Result<Self> {
        let name = NameIndex::read(reader)?;
        let n = reader.read_u32::<LE>()?;
        let type_: Type = (n << 29 >> 29).try_into()?;
        let index = n >> 3;
        Ok(Pair { name, type_, index })
    }
}
impl<W: Write> Writable<W> for Pair {
    fn write(&self, writer: &mut W) -> Result<()> {
        self.name.write(writer)?;
        writer.write_u32::<LE>((self.type_ as u32) | self.index << 3)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub struct MapHandle {
    pub has_numberless_keys: bool,
    pub num: u16,
    pub pair_begin: u32,
}
impl<R: Read> Readable<R> for MapHandle {
    fn read(reader: &mut R) -> Result<Self> {
        let n = reader.read_u64::<LE>()?;
        Ok(Self {
            has_numberless_keys: (n >> 63) != 0,
            num: (n >> 32) as u16,
            pair_begin: n as u32,
        })
    }
}
impl<W: Write> Writable<W> for MapHandle {
    fn write(&self, writer: &mut W) -> Result<()> {
        writer.write_u64::<LE>(
            (self.has_numberless_keys as u64) << 63
                | ((self.num as u64) << 32)
                | (self.pair_begin as u64),
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Type {
    AnsiString = 0,
    WideString = 1,
    NumberlessName = 2,
    Name = 3,
    NumberlessExportPath = 4,
    ExportPath = 5,
    LocalizedText = 6,
}
impl TryFrom<u32> for Type {
    type Error = anyhow::Error;

    fn try_from(v: u32) -> Result<Self> {
        match v {
            x if x == Type::AnsiString as u32 => Ok(Type::AnsiString),
            x if x == Type::WideString as u32 => Ok(Type::WideString),
            x if x == Type::NumberlessName as u32 => Ok(Type::NumberlessName),
            x if x == Type::Name as u32 => Ok(Type::Name),
            x if x == Type::NumberlessExportPath as u32 => Ok(Type::NumberlessExportPath),
            x if x == Type::ExportPath as u32 => Ok(Type::ExportPath),
            x if x == Type::LocalizedText as u32 => Ok(Type::LocalizedText),
            _ => Err(anyhow!("invalid AssetRegistry type: {v}")),
        }
    }
}

#[derive(Debug, PartialEq)]
pub struct AssetData {
    pub object_path: NameIndexFlagged,
    pub package_path: NameIndexFlagged,
    pub asset_class: NameIndexFlagged,
    pub package_name: NameIndexFlagged,
    pub asset_name: NameIndexFlagged,
    pub tags: MapHandle,
    pub bundle_count: u32,
    pub chunk_ids: Vec<u32>,
    pub flags: u32,
}
impl<R: Read> Readable<R> for AssetData {
    fn read(reader: &mut R) -> Result<Self> {
        Ok(AssetData {
            object_path: NameIndexFlagged::read(reader)?,
            package_path: NameIndexFlagged::read(reader)?,
            asset_class: NameIndexFlagged::read(reader)?,
            package_name: NameIndexFlagged::read(reader)?,
            asset_name: NameIndexFlagged::read(reader)?,
            tags: MapHandle::read(reader)?,
            bundle_count: reader.read_u32::<LE>()?,
            chunk_ids: read_array(reader.read_u32::<LE>()?, reader, R::read_u32::<LE>)?,
            flags: reader.read_u32::<LE>()?,
        })
    }
}
impl<W: Write> Writable<W> for AssetData {
    fn write(&self, writer: &mut W) -> Result<()> {
        self.object_path.write(writer)?;
        self.package_path.write(writer)?;
        self.asset_class.write(writer)?;
        self.package_name.write(writer)?;
        self.asset_name.write(writer)?;
        self.tags.write(writer)?;
        writer.write_u32::<LE>(self.bundle_count)?;
        writer.write_u32::<LE>(self.chunk_ids.len() as u32)?;
        for c in self.chunk_ids.iter() {
            writer.write_u32::<LE>(*c)?;
        }
        writer.write_u32::<LE>(self.flags)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub struct Dependencies {
    pub dependencies_size: u64,
    pub dependencies: Vec<u32>,
    pub package_data_buffer_size: u32,
}
impl<R: Read> Readable<R> for Dependencies {
    fn read(reader: &mut R) -> Result<Self> {
        Ok(Dependencies {
            dependencies_size: reader.read_u64::<LE>()?,
            dependencies: read_array(reader.read_u32::<LE>()?, reader, R::read_u32::<LE>)?,
            package_data_buffer_size: reader.read_u32::<LE>()?,
        })
    }
}
impl<W: Write> Writable<W> for Dependencies {
    fn write(&self, writer: &mut W) -> Result<()> {
        writer.write_u64::<LE>(self.dependencies_size)?;
        writer.write_u32::<LE>(self.dependencies.len() as u32)?;
        write_array(
            writer,
            &self.dependencies,
            |w, i| Ok(w.write_u32::<LE>(*i)?),
        )?;
        writer.write_u32::<LE>(self.package_data_buffer_size)?;
        Ok(())
    }
}

const MAGIC_START: u32 = 0x12345679;
const MAGIC_END: u32 = 0x87654321;

#[derive(Debug, PartialEq)]
pub struct Store {
    pub pair_count: u32,
    pub texts: Vec<String>,
    pub nbl_names: Vec<NameIndexFlagged>,
    pub names: Vec<NameIndexFlagged>,
    pub nbl_export_paths: Vec<ExportPath>,
    pub export_paths: Vec<ExportPath>,
    pub ansi_strings: Vec<String>,
    pub wide_strings: Vec<String>,
    pub pairs: Vec<Pair>,
}
impl<R: Read> Readable<R> for Store {
    fn read(reader: &mut R) -> Result<Self> {
        let magic = reader.read_u32::<LE>()?;
        if magic != MAGIC_START {
            return Err(anyhow!(
                "AssetRegistry Store: unexpected magic 0x{:08X}, expected 0x{:08X} (unsupported format?)",
                magic,
                MAGIC_START
            ));
        }

        let nbl_names_count = reader.read_u32::<LE>()?;
        let names_count = reader.read_u32::<LE>()?;
        let nbl_export_path_count = reader.read_u32::<LE>()?;
        let export_path_count = reader.read_u32::<LE>()?;
        let texts_count = reader.read_u32::<LE>()?;
        let ansi_strings_count = reader.read_u32::<LE>()?;
        let wide_strings_count = reader.read_u32::<LE>()?;
        let _ansi_string_bytes = reader.read_u32::<LE>()?;
        let _wide_string_bytes = reader.read_u32::<LE>()?;
        let nbl_pair_count = reader.read_u32::<LE>()?;
        let pair_count = reader.read_u32::<LE>()?;

        let _text_bytes = reader.read_u32::<LE>()?;
        let texts = read_array(texts_count, reader, |r| -> Result<String> {
            let mut chars = vec![0; r.read_u32::<LE>()? as usize - 1];
            r.read_exact(&mut chars)?;
            r.read_u8()?;
            Ok(String::from_utf8_lossy(&chars).into_owned())
        })?;

        let nbl_names = read_array(nbl_names_count, reader, NameIndexFlagged::read)?;
        let names = read_array(names_count, reader, NameIndexFlagged::read)?;

        let nbl_export_paths = read_array(nbl_export_path_count, reader, ExportPath::read)?;
        let export_paths = read_array(export_path_count, reader, ExportPath::read)?;

        let _ansi_string_offsets = read_array(ansi_strings_count, reader, R::read_u32::<LE>)?;
        let _wide_string_offets = read_array(wide_strings_count, reader, R::read_u32::<LE>)?;

        let ansi_strings = read_array(ansi_strings_count, reader, |r| -> Result<String> {
            let mut chars = vec![];
            loop {
                let next = r.read_u8()?;
                if next == 0 {
                    break;
                }
                chars.push(next);
            }
            Ok(String::from_utf8_lossy(&chars).into_owned())
        })?;

        let wide_strings = read_array(wide_strings_count, reader, |reader| -> Result<String> {
            let mut chars = vec![];
            loop {
                let next = reader.read_u16::<LE>()?;
                if next == 0 {
                    break;
                }
                chars.push(char::from_u32(next.into()).unwrap_or(char::REPLACEMENT_CHARACTER));
            }
            Ok(chars.iter().collect::<String>())
        })?;

        let pairs = read_array(nbl_pair_count, reader, Pair::read)?;

        assert_eq!(reader.read_u32::<LE>()?, MAGIC_END);
        Ok(Self {
            pair_count,
            texts,
            nbl_names,
            names,
            nbl_export_paths,
            export_paths,
            ansi_strings,
            wide_strings,
            pairs,
        })
    }
}
impl<W: Write> Writable<W> for Store {
    fn write(&self, writer: &mut W) -> Result<()> {
        writer.write_u32::<LE>(MAGIC_START)?;

        writer.write_u32::<LE>(self.nbl_names.len() as u32)?;
        writer.write_u32::<LE>(self.names.len() as u32)?;
        writer.write_u32::<LE>(self.nbl_export_paths.len() as u32)?;
        writer.write_u32::<LE>(self.export_paths.len() as u32)?;
        writer.write_u32::<LE>(self.texts.len() as u32)?;
        writer.write_u32::<LE>(self.ansi_strings.len() as u32)?;
        writer.write_u32::<LE>(self.wide_strings.len() as u32)?;

        writer.write_u32::<LE>(self.ansi_strings.iter().map(|n| n.len() as u32 + 1).sum())?;
        writer.write_u32::<LE>(
            self.wide_strings
                .iter()
                .map(|n| n.chars().count() as u32 + 1)
                .sum(),
        )?;

        writer.write_u32::<LE>(self.pairs.len() as u32)?;
        writer.write_u32::<LE>(self.pair_count)?;
        writer.write_u32::<LE>(self.texts.iter().map(|n| n.len() as u32 + 1 + 4).sum())?;

        write_array(writer, &self.texts, |w, i| {
            w.write_u32::<LE>(i.len() as u32 + 1)?;
            w.write_all(i.as_bytes())?;
            w.write_u8(0)?;
            Ok(())
        })?;

        write_array(writer, &self.nbl_names, |w, i| i.write(w))?;
        write_array(writer, &self.names, |w, i| i.write(w))?;
        write_array(writer, &self.nbl_export_paths, |w, i| i.write(w))?;
        write_array(writer, &self.export_paths, |w, i| i.write(w))?;

        let mut offset = 0;
        for i in &self.ansi_strings {
            writer.write_u32::<LE>(offset)?;
            offset += i.len() as u32 + 1;
        }

        let mut offset = 0;
        for i in &self.wide_strings {
            writer.write_u32::<LE>(offset)?;
            offset += i.chars().count() as u32 + 1;
        }

        write_array(writer, &self.ansi_strings, |w, i| {
            w.write_all(i.as_bytes())?;
            w.write_u8(0)?;
            Ok(())
        })?;

        write_array(writer, &self.wide_strings, |w, i| {
            for c in i.chars() {
                w.write_u16::<LE>(c as u16)?;
            }
            w.write_u16::<LE>(0)?;
            Ok(())
        })?;

        write_array(writer, &self.pairs, |w, i| i.write(w))?;

        writer.write_u32::<LE>(MAGIC_END)?;
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
pub struct Names(pub indexmap::IndexSet<String>);
impl std::ops::Index<NameIndexFlagged> for Names {
    type Output = String;
    fn index(&self, index: NameIndexFlagged) -> &Self::Output {
        &self.0[index.0 as usize]
    }
}
impl std::ops::Index<NameIndex> for Names {
    type Output = String;
    fn index(&self, index: NameIndex) -> &Self::Output {
        &self.0[index.0 as usize]
    }
}

/// UE 5.6 兼容：UTF-16 使用宽松解码，lone surrogate 替换为 U+FFFD
fn read_string<R: Read>(reader: &mut R, len: i16) -> Result<String> {
    if len < 0 {
        let chars = read_array((len - i16::MIN) as u32, reader, |r| r.read_u16::<LE>())?;
        let length = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
        Ok(utf16_lenient(&chars[..length]))
    } else {
        let mut chars = vec![0; len as usize];
        reader.read_exact(&mut chars)?;
        let length = chars.iter().position(|&c| c == 0).unwrap_or(chars.len());
        Ok(String::from_utf8(chars[..length].to_vec())?)
    }
}
fn write_string<W: Write>(writer: &mut W, string: &str) -> Result<()> {
    if string.is_ascii() {
        writer.write_all(string.as_bytes())?;
    } else {
        for c in string.encode_utf16() {
            writer.write_u16::<LE>(c)?;
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq)]
pub struct AssetRegistry {
    pub version: Guid,
    pub version_int: u32,
    pub hash_version: u64,
    pub names: Names,
    pub store: Store,
    pub asset_data: Vec<AssetData>,
    pub dependencies: Dependencies,
}

impl AssetRegistry {
    /// Creates a structurally valid empty registry for isolated pack/build fixtures.
    pub fn empty() -> Self {
        Self {
            version: [0; 16],
            version_int: 0,
            hash_version: 0,
            names: Names(indexmap::IndexSet::new()),
            store: Store {
                pair_count: 0,
                texts: Vec::new(),
                nbl_names: Vec::new(),
                names: Vec::new(),
                nbl_export_paths: Vec::new(),
                export_paths: Vec::new(),
                ansi_strings: Vec::new(),
                wide_strings: Vec::new(),
                pairs: Vec::new(),
            },
            asset_data: Vec::new(),
            dependencies: Dependencies {
                dependencies_size: 0,
                dependencies: Vec::new(),
                package_data_buffer_size: 0,
            },
        }
    }
}
impl<R: Read> Readable<R> for AssetRegistry {
    fn read(reader: &mut R) -> Result<Self> {
        let version = Guid::read(reader)?;

        let version_int = reader.read_u32::<LE>()?;
        let name_count = reader.read_u32::<LE>()?;
        let _num_string_bytes = reader.read_u32::<LE>()?;
        let hash_version = reader.read_u64::<LE>()?;

        let _lowercase_hashes = read_array(name_count, reader, R::read_u64::<LE>)?;
        let name_lengths = read_array(name_count, reader, R::read_i16::<BE>)?;

        let names = Names(
            name_lengths
                .clone()
                .into_iter()
                .map(|l| read_string(reader, l))
                .collect::<Result<_>>()?,
        );

        let store = Store::read(reader)?;

        let asset_data = read_array(reader.read_u32::<LE>()?, reader, AssetData::read)?;

        let dependencies = Dependencies::read(reader)?;
        Ok(AssetRegistry {
            version,
            version_int,
            hash_version,
            names,
            store,
            asset_data,
            dependencies,
        })
    }
}
impl<W: Write> Writable<W> for AssetRegistry {
    fn write(&self, writer: &mut W) -> Result<()> {
        self.version.write(writer)?;

        fn size<S: AsRef<str>>(s: S) -> u32 {
            let s = s.as_ref();
            if s.is_ascii() {
                s.len() as u32
            } else {
                s.encode_utf16().count() as u32 * 2
            }
        }

        writer.write_u32::<LE>(self.version_int)?;
        writer.write_u32::<LE>(self.names.0.len() as u32)?;
        writer.write_u32::<LE>(self.names.0.iter().map(size).sum())?;
        writer.write_u64::<LE>(self.hash_version)?;

        write_array(writer, &self.names.0, |w, i| {
            let lower = i.to_ascii_lowercase();
            let hash = if lower.is_ascii() {
                cityhasher::hash(lower.as_bytes())
            } else {
                cityhasher::hash(
                    lower
                        .encode_utf16()
                        .flat_map(|s| s.to_le_bytes())
                        .collect::<Vec<u8>>(),
                )
            };
            Ok(w.write_u64::<LE>(hash)?)
        })?;
        write_array(writer, &self.names.0, |w, i| {
            let len = if i.is_ascii() {
                i.len() as i16
            } else {
                i.encode_utf16().count() as i16 + i16::MIN
            };
            Ok(w.write_i16::<BE>(len)?)
        })?;

        write_array(writer, &self.names.0, |w, i| write_string(w, i))?;

        self.store.write(writer)?;

        writer.write_u32::<LE>(self.asset_data.len() as u32)?;
        write_array(writer, &self.asset_data, |w, i| i.write(w))?;

        self.dependencies.write(writer)?;

        Ok(())
    }
}

pub fn get_root_export<C: Read + Seek>(
    asset: &unreal_asset::asset::Asset<C>,
) -> Option<PackageIndex> {
    for (i, e) in asset.asset_data.exports.iter().enumerate() {
        let base = e.get_base_export();
        if base.outer_index.index == 0 && base.object_flags.contains(EObjectFlags::RF_PUBLIC) {
            return Some(PackageIndex::from_export(i as i32).unwrap());
        }
    }
    None
}

impl AssetRegistry {
    pub fn get_name(&mut self, name: &str) -> NameIndexFlagged {
        if let Some(i) = self.names.0.get_index_of(name) {
            NameIndexFlagged(i as u32, None)
        } else {
            self.names.0.insert(name.to_string());
            NameIndexFlagged(self.names.0.len() as u32 - 1, None)
        }
    }
    pub fn populate<C: Read + Seek>(
        &mut self,
        path: &str,
        asset: &unreal_asset::Asset<C>,
    ) -> Result<()> {
        let game_path = super::paths::PakPathBuf::from(
            pak_path_to_game_path(path).context("failed to get game path")?,
        );

        let root = get_root_export(asset).context("no root export")?;
        let root = asset.get_export(root).unwrap();

        let asset_name_str = root.get_base_export().object_name.get_owned_content();
        let package_path_str = game_path.parent().context("no path parent")?.as_str();
        let package_name_str = game_path.as_str();
        let object_path_str = format!("{game_path}.{asset_name_str}");
        let asset_class_str = asset
            .get_import(root.get_base_export().class_index)
            .context("bad import ref")?
            .object_name
            .get_owned_content();

        if let Some(name_index) = self.names.0.get_index_of(&object_path_str) {
            if self
                .asset_data
                .iter()
                .any(|a| a.object_path.0 as usize == name_index)
            {
                return Ok(());
            }
        }

        let object_path = self.get_name(&object_path_str);
        let package_path = self.get_name(package_path_str);
        let asset_class = self.get_name(&asset_class_str);
        let package_name = self.get_name(package_name_str);
        let asset_name = self.get_name(&asset_name_str);

        let new = AssetData {
            object_path,
            package_path,
            asset_class,
            package_name,
            asset_name,
            tags: MapHandle {
                has_numberless_keys: true,
                num: 0,
                pair_begin: 0,
            },
            bundle_count: 0,
            chunk_ids: vec![],
            flags: 0,
        };
        self.asset_data.push(new);

        if let (Some(asset_name_str), Some(object_path_str), Some(asset_class_str)) = (
            asset_name_str.strip_suffix("_C"),
            object_path_str.strip_suffix("_C"),
            asset_class_str.strip_suffix("GeneratedClass"),
        ) {
            let new = AssetData {
                object_path: self.get_name(object_path_str),
                package_path,
                asset_class: self.get_name(asset_class_str),
                package_name,
                asset_name: self.get_name(asset_name_str),
                tags: MapHandle {
                    has_numberless_keys: true,
                    num: 0,
                    pair_begin: 0,
                },
                bundle_count: 0,
                chunk_ids: vec![],
                flags: 0,
            };
            self.asset_data.push(new);
        }

        Ok(())
    }
}
