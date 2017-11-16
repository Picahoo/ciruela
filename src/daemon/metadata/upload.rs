use std::io::{BufReader, BufWriter};
use std::fs::File;
use std::collections::hash_map::Entry;
use std::sync::Arc;

use serde::Serialize;
use serde_cbor::de::from_reader as read_cbor;
use serde_cbor::error::Error as CborError;
use serde_cbor::ser::Serializer as Cbor;

use virtual_path::VPath;
use database::signatures::{State, SignatureEntry};
use proto::{AppendDir};
use proto::{ReplaceDir};
use proto::{SigData, Signature, verify};
use config::Directory;
use metadata::keys::read_upload_keys;
use metadata::{Meta, Error, Writing};

#[derive(Debug, Clone, Copy)]
pub enum Accept {
    New,
    InProgress,
    AlreadyDone,
}

#[derive(Debug, Clone, Copy)]
pub enum Upload {
    Accepted(Accept),
    Rejected(&'static str),
}

fn sort_signatures(new: &mut Vec<SignatureEntry>) {
    new.sort();
}

fn append_signatures(old: &mut Vec<SignatureEntry>,
                     new: Vec<SignatureEntry>)
{
    for sig in new.into_iter() {
        if old.iter().find(|&x| *x == sig).is_none() {
            old.push(sig);
        }
    }
    sort_signatures(old);
}

pub fn read_state(f: File) -> Result<State, CborError> {
    read_cbor(&mut BufReader::new(f))
}

fn check_keys(sigdata: &SigData, signatures: &Vec<Signature>,
              config: &Arc<Directory>, meta: &Meta)
    -> Result<bool, Error>
{
    let keys = read_upload_keys(config, meta)?;
    // If at least one key is allowed we keep all signatures for the key
    // some server needs them
    debug!("Keys {:?} resulted into {} keys read",
        config.upload_keys, keys.len());
    Ok(signatures.iter().any(|sig| verify(sigdata, sig, &keys)))
}

pub fn start_append(params: AppendDir, meta: &Meta)
    -> Result<Upload, Error>
{
    let vpath = params.path.clone();
    let config = if let Some(cfg) = meta.0.config.dirs.get(vpath.key()) {
        if vpath.level() != cfg.num_levels {
            return Err(Error::LevelMismatch(vpath.level(), cfg.num_levels));
        }
        cfg
    } else {
        return Err(Error::PathNotFound(vpath));
    };

    if !check_keys(&params.sig_data(), &params.signatures, config, meta)? {
        warn!("{:?} has no valid signatures. Upload-keys: {:?}",
              params, config.upload_keys);
        return Ok(Upload::Rejected("signature_mismatch"));
    }

    let dir = meta.signatures()?.ensure_dir(vpath.parent_rel())?;

    let timestamp = params.timestamp;
    let mut signatures = params.signatures.into_iter()
        .map(|sig| SignatureEntry {
            timestamp: timestamp,
            signature: sig,
        }).collect::<Vec<_>>();
    sort_signatures(&mut signatures);
    let state_file = format!("{}.state", vpath.final_name());
    let new_state_file = format!("{}.new.state", vpath.final_name());

    let mut writing = meta.writing();
    let (state, new) = match writing.entry(vpath.clone()) {
        Entry::Vacant(e) => {
            if let Some(mut state) = dir.read_file(&state_file, read_state)?
            {
                if state.image == params.image {
                    append_signatures(&mut state.signatures, signatures);
                    dir.replace_file(&state_file, |file| {
                        state.serialize(&mut Cbor::new(BufWriter::new(file)))
                    })?;
                    return Ok(Upload::Accepted(Accept::AlreadyDone));
                } else {
                    return Ok(Upload::Rejected("already_exists"));
                }
            } else {
                let state = State {
                    image: params.image.clone(),
                    signatures: signatures,
                };
                e.insert(Writing {
                    image: state.image.clone(),
                    signatures: state.signatures.clone(),
                    replacing: false,
                });
                (state, Accept::New)
            }
        }
        Entry::Occupied(mut e) => {
            let old_state = e.get_mut();
            if old_state.image == params.image {
                if signatures != old_state.signatures {
                    append_signatures(&mut old_state.signatures, signatures);
                }
                (State {
                    image: old_state.image.clone(),
                    signatures: old_state.signatures.clone(),
                }, Accept::InProgress)
            } else {
                return Ok(Upload::Rejected(
                        "already_uploading_different_version"));
            }
        }
    };
    dir.replace_file(&new_state_file, |file| {
        state.serialize(&mut Cbor::new(BufWriter::new(file)))
    })?;
    Ok(Upload::Accepted(new))
}

pub fn start_replace(params: ReplaceDir, meta: &Meta)
    -> Result<Upload, Error>
{
    let vpath = params.path.clone();
    let config = if let Some(cfg) = meta.0.config.dirs.get(vpath.key()) {
        if vpath.level() != cfg.num_levels {
            return Err(Error::LevelMismatch(vpath.level(), cfg.num_levels));
        }
        cfg
    } else {
        return Err(Error::PathNotFound(vpath));
    };
    if config.append_only {
        return Ok(Upload::Rejected("dir_is_append_only"));
    }

    if !check_keys(&params.sig_data(), &params.signatures, config, meta)? {
        return Ok(Upload::Rejected("signature_mismatch"));
    }

    let dir = meta.signatures()?.ensure_dir(vpath.parent_rel())?;

    let timestamp = params.timestamp;
    let mut signatures = params.signatures.into_iter()
        .map(|sig| SignatureEntry {
            timestamp: timestamp,
            signature: sig,
        }).collect::<Vec<_>>();
    sort_signatures(&mut signatures);
    let state_file = format!("{}.state", vpath.final_name());
    let new_state_file = format!("{}.new.state", vpath.final_name());

    let mut writing = meta.writing();
    let (state, new) = match writing.entry(vpath.clone()) {
        Entry::Vacant(e) => {
            if let Some(mut state) = dir.read_file(&state_file, read_state)? {
                if state.image == params.image {
                    append_signatures(&mut state.signatures, signatures);
                    dir.replace_file(&state_file, |file| {
                        state.serialize(&mut Cbor::new(BufWriter::new(file)))
                    })?;
                    return Ok(Upload::Accepted(Accept::AlreadyDone));
                } else {
                    let state = State {
                        image: params.image.clone(),
                        signatures: signatures,
                    };
                    e.insert(Writing {
                        image: state.image.clone(),
                        signatures: state.signatures.clone(),
                        replacing: true,
                    });
                    (state, Accept::New)
                }
            } else {
                let state = State {
                    image: params.image.clone(),
                    signatures: signatures,
                };
                e.insert(Writing {
                    image: state.image.clone(),
                    signatures: state.signatures.clone(),
                    replacing: false,
                });
                (state, Accept::New)
            }
        }
        Entry::Occupied(mut e) => {
            let old_state = e.get_mut();
            if old_state.image == params.image {
                if signatures != old_state.signatures {
                    append_signatures(&mut old_state.signatures, signatures);
                }
                (State {
                    image: old_state.image.clone(),
                    signatures: old_state.signatures.clone(),
                }, Accept::InProgress)
            } else {
                // TODO(tailhook) stop fetching image, delete and
                // start replacing
                warn!("Replace is rejected because already in progress");
                return Ok(Upload::Rejected(
                    "already_uploading_different_version"));
            }
        }
    };
    dir.replace_file(&new_state_file, |file| {
        state.serialize(&mut Cbor::new(BufWriter::new(file)))
    })?;
    Ok(Upload::Accepted(new))
}

pub(in metadata) fn abort_dir(vpath: &VPath, _wr: Writing, meta: &Meta)
    -> Result<(), Error>
{
    let new_state_file = format!("{}.new.state", vpath.final_name());
    let dir = meta.signatures()?.ensure_dir(vpath.parent_rel())?;
    dir.remove_file(&new_state_file)?;
    Ok(())
}

pub(in metadata) fn commit_dir(vpath: &VPath, _wr: Writing, meta: &Meta)
    -> Result<(), Error>
{
    let state_file = format!("{}.state", vpath.final_name());
    let new_state_file = format!("{}.new.state", vpath.final_name());
    let dir = meta.signatures()?.ensure_dir(vpath.parent_rel())?;
    dir.rename(&new_state_file, &state_file)?;
    Ok(())
}
