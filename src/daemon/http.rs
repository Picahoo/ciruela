use std::collections::BTreeMap;
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime};

use futures::Async;
use futures::future::{Future, FutureResult, Either, ok};
use futures::stream::Stream;
use libcantal::{Json, Collection};
use self_meter_http::{Meter, ProcessReport, ThreadReport};
use serde::Serialize;
use serde_json;
use time;
use tk_bufstream::{ReadBuf, WriteBuf};
use tk_easyloop::{spawn, handle};
use tk_easyloop;
use tk_http::Status;
use tk_http::server::{WebsocketHandshake, Head, RecvMode};
use tk_http::server::{self, Proto, Encoder, EncoderDone, Error, Config};
use tk_http::websocket::ServerCodec;
use tk_listen::ListenExt;
use tokio_core::net::TcpListener;
use tokio_io::{AsyncRead, AsyncWrite};

use ciruela::VPath;
use machine_id::MachineId;
use mask::Mask;
use metrics;
use remote::websocket::Connection;
use tracking::Tracking;


const BODY: &'static str = "Not found";
const BAD_REQUEST: &'static str = "Bad Request";

struct HttpCodec {
    addr: SocketAddr,
    route: Option<Route>,
    meter: Meter,
    tracking: Tracking,
}

struct Dispatcher {
    addr: SocketAddr,
    meter: Meter,
    tracking: Tracking,
}

enum ClusterRoute {
    Downloading,
    Deleted,
    Complete,
    Watching,
    ByDir,
    Configs,
}

enum Route {
    NotFound,
    BadRequest,
    Websocket(WebsocketHandshake),
    Status,
    BaseDirs,
    Configs,
    Cluster(ClusterRoute),
    Downloading,
    Deleted,
    Watching,
    Watched,
    Peers,
    ListDir(VPath),
}


impl<S> server::Dispatcher<S> for Dispatcher
    where S: AsyncRead + AsyncWrite + 'static,
{
    type Codec = HttpCodec;

    fn headers_received(&mut self, headers: &Head)
        -> Result<Self::Codec, Error>
    {
        match headers.get_websocket_upgrade() {
            Ok(up) => {
                Ok(HttpCodec {
                    addr: self.addr,
                    route: Some(
                        Route::parse(headers.path().unwrap_or("/"), up)),
                    meter: self.meter.clone(),
                    tracking: self.tracking.clone(),
                })
            }
            Err(()) => {
                Ok(HttpCodec {
                    addr: self.addr,
                    route: Some(Route::BadRequest),
                    meter: self.meter.clone(),
                    tracking: self.tracking.clone(),
                })
            }
        }
    }
}


impl<S> server::Codec<S> for HttpCodec
    where S: AsyncRead + AsyncWrite + 'static,
{
    type ResponseFuture = Either<
        FutureResult<EncoderDone<S>, Error>,
        Box<Future<Item=EncoderDone<S>, Error=Error>>,
    >;
    fn recv_mode(&mut self) -> RecvMode {
        if let Some(Route::Websocket(..)) = self.route {
            RecvMode::hijack()
        } else {
            RecvMode::buffered_upfront(0)
        }
    }
    fn data_received(&mut self, data: &[u8], end: bool)
        -> Result<Async<usize>, Error>
    {
        debug_assert!(end);
        debug_assert_eq!(data.len(), 0);
        Ok(Async::Ready(data.len()))
    }
    fn start_response(&mut self, mut e: Encoder<S>) -> Self::ResponseFuture {
        match self.route.take().unwrap() {
            Route::Websocket(ws) => {
                e.status(Status::SwitchingProtocol);
                e.format_header("Date", time::now_utc().rfc822()).unwrap();
                e.add_header("Server",
                    concat!("ciruela/", env!("CARGO_PKG_VERSION"))
                ).unwrap();
                e.add_header("Connection", "upgrade").unwrap();
                e.add_header("Upgrade", "websocket").unwrap();
                e.format_header("Sec-Websocket-Accept", &ws.accept).unwrap();
                e.done_headers().unwrap();
                Either::A(ok(e.done()))
            }
            Route::Status => {
                #[derive(Serialize)]
                struct Report<'a> {
                    machine_id: &'a MachineId,
                    hostname: &'a str,
                    version: &'static str,
                    process: ProcessReport<'a>,
                    threads: ThreadReport<'a>,
                    metrics: Json<'a, Vec<Box<Collection>>>,
                }
                Either::A(ok(serve_json(e, &Report {
                    machine_id: &self.tracking.config().machine_id,
                    hostname: &self.tracking.config().hostname,
                    version: env!("CARGO_PKG_VERSION"),
                    process: self.meter.process_report(),
                    threads: self.meter.thread_report(),
                    metrics: Json(&metrics::all()),
                })))
            }
            Route::Downloading => {
                #[derive(Serialize)]
                pub struct Progress {
                    pub image_id: String,
                    pub mask: Mask,
                    pub stalled: bool,
                    pub source: bool,
                }
                Either::A(ok(serve_json(e, &self.tracking.get_in_progress()
                    .iter().map(|(path, p)| (path, Progress {
                        image_id: format!("{}", p.image_id),
                        mask: p.mask,
                        stalled: p.stalled,
                        source: p.source,
                    })).collect::<BTreeMap<_, _>>())))
            }
            Route::BaseDirs => {
                #[derive(Serialize)]
                pub struct BaseDir {
                    pub hash: String,
                    pub num_subdirs: usize,
                    pub num_downloading: usize,
                    #[serde(with="::serialize::timestamp")]
                    pub last_scan: SystemTime,
                }
                Either::A(ok(serve_json(e, &self.tracking.get_base_dirs()
                    .iter().map(|(path, d)| (path, BaseDir {
                        hash: format!("{}", d.hash),
                        num_subdirs: d.num_subdirs,
                        num_downloading: d.num_downloading,
                        last_scan: d.last_scan,
                    })).collect::<BTreeMap<_, _>>())))
            }
            Route::Configs => {
                #[derive(Serialize)]
                pub struct Config {
                    append_only: bool,
                    num_levels: usize,
                    auto_clean: bool,
                }
                Either::A(ok(serve_json(e, &self.tracking.config().dirs
                    .iter().map(|(path, d)| (path, Config {
                        append_only: d.append_only,
                        num_levels: d.num_levels,
                        auto_clean: d.auto_clean,
                    })).collect::<BTreeMap<_, _>>())))
            }
            Route::Deleted => {
                Either::A(ok(serve_json(e, &self.tracking.get_deleted()
                    .iter()
                    .map(|&(ref vpath, ref id)| (vpath.clone(), id.to_string()))
                    .collect::<Vec<_>>())))
            }
            Route::Watching => {
                Either::A(ok(serve_json(e, &self.tracking.get_watching())))
            }
            Route::Watched => {
                Either::A(ok(serve_json(e, &self.tracking.get_watched())))
            }
            Route::Peers => {
                Either::A(ok(serve_json(e,
                    &self.tracking.peers().get_peers())))
            }
            Route::ListDir(path) => {
                Either::B(Box::new(self.tracking.meta().scan_dir(&path)
                    .map_err(|e| Error::custom(e.to_string()))
                    .and_then(|data| {
                        Ok(serve_json(e, &data))
                    })))
            }
            Route::Cluster(ClusterRoute::Downloading) => {
                #[derive(Serialize)]
                pub struct Progress {
                    pub image_id: String,
                    pub mask: Mask,
                    pub stalled: bool,
                    pub source: bool,
                }
                let dl = &*self.tracking.peers().get_host_data();
                Either::A(ok(serve_json(e, &dl
                    .iter().map(|(machine_id, data)| {
                        (format!("{}", machine_id), data.downloading.iter()
                            .map(|(path, p)| (path, Progress {
                                image_id: format!("{}", p.image),
                                mask: p.mask,
                                stalled: p.stalled,
                                source: p.source,
                            })).collect::<BTreeMap<_, _>>())
                    }).collect::<BTreeMap<_, _>>())))
            }
            Route::Cluster(ClusterRoute::Deleted) => {
                let dl = &*self.tracking.peers().get_host_data();
                Either::A(ok(serve_json(e, &dl
                    .iter().map(|(machine_id, data)| {
                        (format!("{}", machine_id), data.deleted.iter()
                            .map(|&(ref vpath, ref id)| {
                                (vpath.clone(), id.to_string())
                            })
                            .collect::<Vec<_>>())
                    }).collect::<BTreeMap<_, _>>())))
            }
            Route::Cluster(ClusterRoute::Complete) => {
                let dl = &*self.tracking.peers().get_host_data();
                Either::A(ok(serve_json(e, &dl
                    .iter().map(|(machine_id, data)| {
                        (format!("{}", machine_id), data.complete.iter()
                            .map(|(ref vpath, ref id)| {
                                (vpath.clone(), id.to_string())
                            })
                            .collect::<Vec<_>>())
                    }).collect::<BTreeMap<_, _>>())))
            }
            Route::Cluster(ClusterRoute::Watching) => {
                let hdata = &*self.tracking.peers().get_host_data();
                Either::A(ok(serve_json(e, &hdata
                    .iter().map(|(machine_id, data)| {
                        (format!("{}", machine_id), &data.watching)
                    }).collect::<BTreeMap<_, _>>())))
            }
            Route::Cluster(ClusterRoute::ByDir) => {
                let configs = &*self.tracking.peers().get_configs();
                Either::A(ok(serve_json(e, &configs.all_dirs())))
            }
            Route::Cluster(ClusterRoute::Configs) => {
                let configs = &*self.tracking.peers().get_configs();
                Either::A(ok(serve_json(e, &configs.all_hosts())))
            }
            Route::BadRequest => {
                e.status(Status::BadRequest);
                e.add_length(BAD_REQUEST.as_bytes().len() as u64).unwrap();
                e.format_header("Date", time::now_utc().rfc822()).unwrap();
                e.add_header("Server",
                    concat!("ciruela/", env!("CARGO_PKG_VERSION"))
                ).unwrap();
                if e.done_headers().unwrap() {
                    e.write_body(BAD_REQUEST.as_bytes());
                }
                Either::A(ok(e.done()))
            }
            Route::NotFound => {
                e.status(Status::NotFound);
                e.add_length(BODY.as_bytes().len() as u64).unwrap();
                e.format_header("Date", time::now_utc().rfc822()).unwrap();
                e.add_header("Server",
                    concat!("ciruela/", env!("CARGO_PKG_VERSION"))
                ).unwrap();
                if e.done_headers().unwrap() {
                    e.write_body(BODY.as_bytes());
                }
                Either::A(ok(e.done()))
            }
        }
    }
    fn hijack(&mut self, write_buf: WriteBuf<S>, read_buf: ReadBuf<S>){
        let inp = read_buf.framed(ServerCodec);
        let out = write_buf.framed(ServerCodec);
        let (cli, fut) = Connection::incoming(
            self.addr, out, inp, &self.tracking.remote(), &self.tracking);
        let token = self.tracking.remote().register_connection(&cli);
        spawn(fut
            .map_err(|e| debug!("websocket closed: {}", e))
            .then(move |r| {
                drop(token);
                r
            }));
    }
}

fn serve_json<S, V: Serialize>(mut e: Encoder<S>, data: &V)
    -> EncoderDone<S>
{
    e.status(Status::Ok);
    e.add_chunked().unwrap();
    e.format_header("Date", time::now_utc().rfc822()).unwrap();
    e.add_header("Content-Type", "application/json").unwrap();
    e.add_header("Server",
        concat!("ciruela/", env!("CARGO_PKG_VERSION"))
    ).unwrap();
    if e.done_headers().unwrap() {
        serde_json::to_writer(io::BufWriter::new(&mut e), data)
            .expect("can always serialize");
    }
    e.done()
}

impl Route {
    fn parse(path: &str, wh: Option<WebsocketHandshake>) -> Route {
        if path == "/" {
            if let Some(wh) = wh {
                return Route::Websocket(wh);
            } else {
                return Route::NotFound;
            }
        } else if path == "/status/" {
            return Route::Status;
        } else if path == "/downloading/" {
            return Route::Downloading;
        } else if path == "/base-dirs/" {
            return Route::BaseDirs;
        } else if path == "/configs/" {
            return Route::Configs;
        } else if path == "/deleted/" {
            return Route::Deleted;
        } else if path == "/watching/" {
            return Route::Watching;
        } else if path == "/watched/" {
            return Route::Watched;
        } else if path == "/peers/" {
            return Route::Peers;
        } else if path.starts_with("/list-dir/") {
            let subpath = &path["/list-dir".len()..];
            match VPath::try_from(subpath) {
                Ok(path) => return Route::ListDir(path),
                Err(_) => return Route::NotFound,
            }
        } else if path.starts_with("/cluster/") {
            if path == "/cluster/downloading/" {
                return Route::Cluster(ClusterRoute::Downloading);
            } else if path == "/cluster/complete/" {
                return Route::Cluster(ClusterRoute::Complete);
            } else if path == "/cluster/deleted/" {
                return Route::Cluster(ClusterRoute::Deleted);
            } else if path == "/cluster/dirs/" {
                return Route::Cluster(ClusterRoute::ByDir);
            } else if path == "/cluster/configs/" {
                return Route::Cluster(ClusterRoute::Configs);
            } else if path == "/cluster/watching/" {
                return Route::Cluster(ClusterRoute::Watching);
            } else {
                return Route::NotFound;
            }
        } else{
            return Route::NotFound;
        }
    }
}


pub fn start(addr: SocketAddr, tracking: &Tracking, meter: &Meter)
    -> Result<(), io::Error>
{
    let listener = TcpListener::bind(&addr, &handle())?;
    let cfg = Config::new().done();
    let tracking = tracking.clone();
    let meter = meter.clone();

    spawn(listener.incoming()
        .sleep_on_error(Duration::from_millis(100), &tk_easyloop::handle())
        .map(move |(socket, addr)| {
            Proto::new(socket, &cfg, Dispatcher {
                addr: addr,
                meter: meter.clone(),
                tracking: tracking.clone(),
                }, &tk_easyloop::handle())
            .map_err(|e| { debug!("Connection error: {}", e); })
        })
        .listen(1000)
        .then(|value| -> Result<(), ()> {
            error!("Unexpected closure of listening socket: {:?}", value);
            panic!("Listening socket closed unexpectedly");
        }));
    Ok(())
}
