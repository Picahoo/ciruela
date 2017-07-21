use std::io::{BufReader, Write};

use hex::ToHex;

use ciruela::ImageId;
use index::Index;
use metadata::{Meta, Error};


pub fn write(id: &ImageId, data: Vec<u8>, meta: &Meta)
    -> Result<(), Error>
{
    // TODO(tailhook) assert on thread name
    let hex_id = id.to_hex();
    let filename = format!("{}.ds1", &hex_id);
    let base = meta.indexes()?.ensure_dir(&hex_id[..2])?;
    base.replace_file(&filename, |mut f| f.write_all(&data))?;
    Ok(())
}