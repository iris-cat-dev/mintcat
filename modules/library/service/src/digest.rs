use std::io::{self, Write};
use serde::Serialize;
use sha2::{Digest, Sha256};
use crate::LibraryError;
pub(crate) struct DigestWriter<W> {pub inner:W,hash:Sha256}
impl<W:Write> DigestWriter<W> {
    pub fn new(inner:W)->Self {Self {inner,hash:Sha256::new()}}
    pub fn finish(self)->(W,String) {(self.inner,format!("sha256:{:x}",self.hash.finalize()))}
}
impl<W:Write> Write for DigestWriter<W> {
    fn write(&mut self,bytes:&[u8])->io::Result<usize> {let count=self.inner.write(bytes)?;self.hash.update(&bytes[..count]);Ok(count)}
    fn flush(&mut self)->io::Result<()> {self.inner.flush()}
}
pub(crate) fn serialized_digest<T:Serialize>(value:&T)->Result<String,LibraryError> {
    let mut writer=DigestWriter::new(io::sink());serde_json::to_writer(&mut writer,value)?;Ok(writer.finish().1)
}
