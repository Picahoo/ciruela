use std::io;
use std::path::{PathBuf};
use serde_cbor;
use dir_signature::v1::{ParseError as IndexError};
use serde_cbor::error::Error as CborError;

use ciruela::VPath;


quick_error! {
    #[derive(Debug)]
    pub enum Error {
        InvalidPath {
            description("invalid path \
                (not absolute or has parents or invalid utf8)")
        }
        PathNotFound(path: VPath) {
            description("path not found")
            display("destination path for {:?} is not found", path)
        }
        LevelMismatch(has: usize, required: usize) {
            description("invalid directory level in upload path")
            display("expected path with {} components, but is {}",
                    required, has)
        }
        OpenRoot(dir: PathBuf, e: io::Error) {
            description("can't open root metadata dir")
            display("can't open root metadata dir {:?}: {}", dir, e)
            cause(e)
        }
        CreateDirRace(dir: PathBuf, e: io::Error) {
            description("race condition when creating metadata dir")
            display("race condition when creating metadata dir {:?}: {}",
                    dir, e)
            cause(e)
        }
        OpenMeta(dir: PathBuf, e: io::Error) {
            description("can't open metadata dir")
            display("can't open metadata dir {:?}: {}", dir, e)
            cause(e)
        }
        Read(dir: PathBuf, e: io::Error) {
            description("can't open metadata file")
            display("can't open metadata file {:?}: {}", dir, e)
            cause(e)
        }
        Encode(dir: PathBuf, e: CborError) {
            description("can't encode metadata file")
            display("can't encode metadata file {:?}: {}", dir, e)
            cause(e)
        }
        Decode(dir: PathBuf, e: Box<::std::error::Error + Send>) {
            description("can't decode metadata file")
            display("can't decode metadata file {:?}: {}", dir, e)
            cause(&**e)
        }
        ListDir(dir: PathBuf, e: io::Error) {
            description("can't list metadata dir")
            display("can't list metadata dir {:?}: {}", dir, e)
            cause(e)
        }
        CreateDir(dir: PathBuf, e: io::Error) {
            description("can't create metadata dir")
            display("can't create metadata dir {:?}: {}", dir, e)
            cause(e)
        }
        WriteMeta(dir: PathBuf, e: io::Error) {
            description("can't write metadata file")
            display("can't write metadata file {:?}: {}", dir, e)
            cause(e)
        }
        SerializeError(e: serde_cbor::Error) {
            description("can't serialize metadata")
            display("can't serialize metadata: {}", e)
            cause(e)
            from()
        }
        BadIndex(path: PathBuf, e: IndexError) {
            description("error reading index")
            display("error reading index: {}", e)
            cause(e)
        }
        IndexNotFound {
            description("index not found")
        }
    }
}
