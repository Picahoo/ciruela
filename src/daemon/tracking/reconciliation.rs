use std::net::SocketAddr;

use ciruela::{VPath, Hash};
use ciruela::proto::{BaseDirState, AppendDir, ReplaceDir, GetBaseDir};
use ciruela::proto::{RequestClient};
use machine_id::{MachineId};
use tracking::Subsystem;

use futures::future::{Future, Loop, loop_fn};
use tk_easyloop::spawn;
use tracking::base_dir;
use metadata::Upload;


pub struct ReconPush {
    pub path: VPath,
    pub hash: Hash,
    pub initial_addr: SocketAddr,
    pub initial_machine_id: MachineId,
}

pub fn start(sys: &Subsystem, info: ReconPush) {
    debug!("Reconciling {:?} to hash {} from {}/{}",
        info.path, info.hash, info.initial_addr, info.initial_machine_id);
    let sys = sys.clone();
    let sys2 = sys.clone();
    let sys3 = sys.clone();
    let sys_drop = sys.clone();
    let ReconPush {
        path,
        hash,
        initial_addr: addr,
        initial_machine_id: mid
    } = info;
    // TODO(tailhook) allow Remote subsystem to pick a connection, so
    // it can choose one, already available when multiple choices are there
    let pair = (path.clone(), hash);
    spawn(loop_fn((addr, mid), move |(addr, mid)| {
        let sys = sys.clone();
        let pair = pair.clone(); // TODO(tailhook) optimize?
        sys.remote.ensure_connected(&sys.tracking, addr)
            .request(GetBaseDir { path: pair.0.clone() })
            .then(move |res| {
                let mut state = sys.state();
                match res {
                    Ok(dir) => {
                        let dir_state = BaseDirState {
                            path: pair.0.clone(),
                            config_hash: dir.config_hash,
                            keep_list_hash: dir.keep_list_hash,
                            dirs: dir.dirs,
                        };
                        let dir_hash = Hash::for_object(&dir_state);
                        if dir_hash == hash {
                            return Ok(Loop::Break((addr, dir_state)))
                        } else {
                            debug!("Mismatching hash from {}:{:?}: {} != {}",
                                addr, pair.0, hash, dir_hash);
                        }
                    }
                    Err(e) => {
                        warn!("Error fetching {} from {}: {}", hash, addr, e);
                    }
                }
                let next_host = state.reconciling
                    .get_mut(&pair)
                    .and_then(|h| {
                        h.remove(&(addr, mid));
                        let item = h.iter().cloned().next();
                        item.as_ref().map(|pair| h.remove(&pair));
                        item
                    });
                if let Some(next_host) = next_host {
                    return Ok(Loop::Continue(next_host))
                } else {
                    // It's fine, probably all hosts have an updated hash already.
                    // It might also be that there is some race condition, like
                    // we tried to do request, and it failed temporarily
                    // (keep-alive connection is dropping). But we didn't mark
                    // this hash as visited, yet so on next ping we will retry.
                    debug!("No next host for {:?}", pair);
                    return Err(());
                }
            })
    })
    .and_then(move |(addr, dir)| {
        let config = sys2.config.dirs.get(dir.path.key())
            .expect("only configured dirs are reconciled");
        base_dir::scan(&dir.path, config, &sys2.meta, &sys2.disk)
        .then(move |result| match result {
            Ok(state) => Ok((addr, dir, state)),
            Err(e) => {
                error!("Error scanning base-dir {:?}: {}", dir.path, e);
                Err(())
            }
        })
    })
    .map(move |(_addr, remote, mut local)| {
        for (name, mut rstate) in remote.dirs {
            let vpath = local.path.join(&name);
            let sys = sys3.clone();
            let sig = match rstate.signatures.pop() {
                Some(x) => x,
                None => {
                    warn!("Got image with no signatures: {:?}", vpath);
                    continue;
                }
            };
            // TODO(tailhook) consume multiple signatures
            let image_id = rstate.image;
            if let Some(old_state) = local.dirs.remove(&name) {
                debug!("Replacing {:?}", vpath);
                spawn(
                    sys.meta.replace_dir(ReplaceDir {
                        path: vpath.clone(),
                        image: image_id.clone(),
                        old_image: Some(old_state.image),
                        timestamp: sig.timestamp,
                        signatures: vec![sig.signature],
                    }).then(move |result| {
                        match result {
                            Ok(Upload { accepted: true, new: true }) => {
                                sys.tracking.fetch_dir(
                                    vpath, image_id, true);
                            }
                            Ok(Upload {..}) => {} // TODO(tailhook) log?
                            Err(e) => {
                                error!("Error reconciling {:?}: {}",
                                    vpath, e);
                            }
                        }
                        Ok(())
                    }));
            } else {
                debug!("Appending {:?}", vpath);
                spawn(
                    sys.meta.append_dir(AppendDir {
                        path: vpath.clone(),
                        image: image_id.clone(),
                        timestamp: sig.timestamp,
                        signatures: vec![sig.signature],
                    }).then(move |result| {
                        match result {
                            Ok(Upload { accepted: true, new: true }) => {
                                sys.tracking.fetch_dir(
                                    vpath, image_id, false);
                            }
                            Ok(Upload {..}) => {} // TODO(tailhook) log?
                            Err(e) => {
                                error!("Error reconciling {:?}: {}",
                                    vpath, e);
                            }
                        }
                        Ok(())
                    }));
            }
        }
        Ok::<(), ()>(())
    })
    .then(move |_| -> Result<(), ()> {
        sys_drop.state().reconciling.remove(&(path, hash));
        Ok(())
    }));
}
