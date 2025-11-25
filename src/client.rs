use std::collections::HashMap;
use std::env;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tokio::sync::Mutex as AMutex;

use tokio::io::AsyncReadExt;
use tokio::select;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::*;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::watch;
use tokio::sync::{Notify, Semaphore, TryAcquireError};
use tokio::task::JoinHandle as Handle;
use tokio::time;
use tokio::time::{Duration, sleep};
use tokio_util::sync::CancellationToken;

use tracing::{instrument};

use crate::{BinResponse,BinInstructs,SQLResponse,SQLInstructs, ProcResp ,ClientResponse, ClientInstruct};


use serde_derive::{Deserialize, Serialize};
use serde_json::Value;

use log::Level;

use config::Config;

//TODO replace with select_all;
use futures::future::join_all;

use eframe::EventLoopBuilderHook;
use eframe::egui;
use winit::platform::wayland::EventLoopBuilderExtWayland;

use crate::binance::BinanceClient;
use crate::data::AssetData;
use crate::data::Intv;
use crate::data::SQLConn;
use crate::gui::DesktopApp;
use crate::binance::SymbolOutput;

use anyhow::{Result,anyhow,Context};
const err_ctx:&str="Main client";

pub fn load_config() -> Result<Config> {
    let settings = Config::builder()
        .add_source(config::File::with_name("./Settings.toml"))
        .build()?;
    Ok(settings)
}

#[derive(Clone, Debug)]
enum Setting {
    Symbol { s: String },
    Intv { i: Intv },
    IP { i: String },
    Port { i: u16 },
}

#[derive(Clone, Debug, Copy)]
enum Tasks {
    Task0Cli { settings: i32 },
    Task1BinWS { settings: i32 },
    Task3SQL { settings: i32 },
}
impl Tasks {
    fn new_cli(global_settings: &Config) -> Tasks {
        let t = Tasks::Task0Cli { settings: 0 };
        return t;
    }
    fn new_binws(global_settings: &Config) -> Tasks {
        let t = Tasks::Task1BinWS { settings: 0 };
        return t;
    }
    fn new_sql(global_settings: &Config) -> Tasks {
        let t = Tasks::Task3SQL { settings: 0 };
        return t;
    }
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct CliMode {
    pub mode: Frontend,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Frontend {
    Desktop,
}
impl Frontend {
    fn init(&self, config: &Config) -> Vec<Tasks> {
        let mut tasks: Vec<Tasks> = std::vec::Vec::new();
        let s0 = Tasks::new_cli(&config);
        tasks.push(s0);
        let s1 = Tasks::new_binws(&config);
        tasks.push(s1);
        let s3 = Tasks::new_sql(&config);
        tasks.push(s3);
        return tasks;
    }
}

#[derive(Debug)]
pub enum ChanType {
    VSend { s: Sender<Value> },
    VRecv { r: Receiver<Value> },

    WSend { s: watch::Sender<Value> },
    WRecv { r: watch::Receiver<Value> },

    WSendCli { s: watch::Sender<ClientInstruct> },
    WRecvCli { r: watch::Receiver<ClientInstruct> },
    WRSendCli { s: watch::Sender<ClientResponse> },
    WRRecvCli { r: watch::Receiver<ClientResponse> },

    BSend { s: watch::Sender<BinInstructs> },
    BRecv { r: watch::Receiver<BinInstructs> },
    BRSend { s: watch::Sender<BinResponse> },
    BRRecv { r: watch::Receiver<BinResponse> },

    SSend { s: watch::Sender<SQLInstructs> },
    SRecv { r: watch::Receiver<SQLInstructs> },
    SRSend { s: watch::Sender<SQLResponse> },
    SRRecv { r: watch::Receiver<SQLResponse> },
}
macro_rules! send_channel_to_self_push_recv_out {
    ($s:ident, $ss:ident, $cv:ident, $enum:ident, $t:ty, $box:ident, $chan:ident, $q:tt) => {
        let (s_sett, r_sett): ($box::Sender<$t>, $box::Receiver<$t>) =
            $box::$chan($q);
        $s.$ss = Some(s_sett);
        let recv_sett = ChanType::$enum { r: r_sett };
        $cv.push(recv_sett);
    };
}
macro_rules! recv_channel_to_self_push_send_out {
    ($s:ident, $ss:ident,  $cv:ident, $enum:ident, $t:ty, $box:ident, $chan:ident, $q:tt) => {
        let (s_sett, r_sett): ($box::Sender<$t>, $box::Receiver<$t>) =
            $box::$chan($q);
        $s.$ss = Some(r_sett);
        let send_sett = ChanType::$enum { s: s_sett };
        $cv.push(send_sett);
    };
}
macro_rules! unpack_channels{
    ( $($input_vec:ident, $send_enum:ident, $message:ident, $recv_enum:ident, $response:ident),* ) => {
        {
            assert_eq!(2,$($input_vec.len())*);
            let channel1:ChanType=$($input_vec)*.pop().expect("Channel doesn't exist!");
            let mut chan1:watch::Sender<$($message)*>;
            match channel1{
                ChanType::$($send_enum)*{s:send} => chan1=send,
                _ =>panic!("Wrong channel type:{:?}",channel1),
            }
            let channel2:ChanType=$($input_vec)*.pop().expect("Channel doesn't exist!");
            let mut chan2:watch::Receiver<$($response)*>;
            match channel2{
                ChanType::$($recv_enum)*{r:recv} => chan2=recv,
                _ =>panic!("Wrong channel type:{:?}",channel2),
            }
            (chan1,chan2)
        }
    };
}
#[derive(Debug)]
pub struct ClientTask {
    frontend: Frontend,

    live_dat: Arc<Mutex<AssetData>>,
    hist_dat: Arc<Mutex<AssetData>>,
    latest_price: Arc<AMutex<f64>>,
    last_price: Arc<Mutex<f64>>,
    live_collect: Arc<Mutex<HashMap<String,SymbolOutput>>>,

    lp_chan_recv:Option<watch::Receiver<f64>>,
    lp_chan_send:Option<watch::Sender<f64>>,

    settings: Config,
    cancel_all: CancellationToken,

    cli_awake: Arc<Notify>,
    cli_sleep: Arc<Notify>,

    //Frontend
    recv_settings: Option<watch::Receiver<ClientInstruct>>,
    send_update: Option<watch::Sender<ClientResponse>>,

    //Task1 = Binance WS
    send_sett_bin: Option<watch::Sender<BinInstructs>>,
    recv_response_bin: Option<watch::Receiver<BinResponse>>,
    bin_awake: Arc<Notify>,
    bin_sleep: Arc<Notify>,

    //Task3 = SQL
    send_sett_sql: Option<watch::Sender<SQLInstructs>>,
    recv_response_sql: Option<watch::Receiver<SQLResponse>>,
    sql_awake: Arc<Notify>,
    sql_sleep: Arc<Notify>,
}

impl ClientTask {
    fn new(f: Frontend, conf: Config) -> Self {
        Self {
            frontend: f,
            live_dat: Arc::new(Mutex::new(AssetData::new(0))),
            hist_dat: Arc::new(Mutex::new(AssetData::new(1))),

            cli_awake: Arc::new(Notify::new()),
            cli_sleep: Arc::new(Notify::new()),

            settings: conf,
            cancel_all: CancellationToken::new(),

            recv_settings: None,
            send_update: None,

            send_sett_bin: None,
            recv_response_bin: None,
            bin_awake: Arc::new(Notify::new()),
            bin_sleep: Arc::new(Notify::new()),
            last_price: Arc::new(Mutex::new(0.0)),
            latest_price: Arc::new(AMutex::new(0.0)),
            live_collect: Arc::new(Mutex::new(HashMap::new())),

            lp_chan_recv:None,
            lp_chan_send:None,

            send_sett_sql: None,
            recv_response_sql: None,
            sql_awake: Arc::new(Notify::new()),
            sql_sleep: Arc::new(Notify::new()),
        }
    }
    fn start_gui(
        mut task_chans: Vec<ChanType>,
        asset_data: Arc<Mutex<AssetData>>,
        hist_asset_data: Arc<Mutex<AssetData>>,
        live_price: Arc<Mutex<f64>>,
        collect_data: Arc<Mutex<HashMap<String,SymbolOutput>>>,
        lp_chan_recv: watch::Receiver<f64>
    ) -> Handle<()> {
        let rchan: watch::Receiver<ClientResponse>;
        let rchan_e = task_chans.pop().expect("Task channels doesn't exist!");
        match rchan_e {
            ChanType::WRRecvCli { r: rr } => rchan = rr,
            _ => panic!("Wrong channel type passed to GUI"),
        }
        let schan: watch::Sender<ClientInstruct>;
        let schan_e = task_chans.pop().expect("Task channels doesn't exist!");
        match schan_e {
            ChanType::WSendCli { s: ss } => schan = ss,
            _ => panic!("Wrong channel type passed to GUI"),
        }
        let handle: Handle<()> = tokio::task::spawn_blocking(move || {
            loop {
                log::info!("Desktop GUI started");
                let event_loop_builder: Option<EventLoopBuilderHook> =
                    Some(Box::new(|event_loop_builder| {
                        event_loop_builder.with_any_thread(true);
                    }));
                let native_options = eframe::NativeOptions {
                    viewport: egui::ViewportBuilder::default()
                        .with_inner_size([1980.0, 1080.0])
                        .with_min_inner_size([300.0, 220.0]),
                    event_loop_builder,
                    persist_window: true,
                    persistence_path: Some(std::path::PathBuf::from(
                        "./bintrade_gui_save",
                    )),
                    ..Default::default()
                };

                let r = eframe::run_native(
                    "Bintrade Gui",
                    native_options,
                    Box::new(|cc| {
                        Ok(Box::new(DesktopApp::new(
                            cc,
                            schan.clone(),
                            rchan.clone(),
                            asset_data.clone(),
                            hist_asset_data.clone(),
                            live_price.clone(),
                            collect_data.clone(),
                            lp_chan_recv.clone()
                        )))
                    }),
                );
                match r {
                    Ok(_a) => return (),
                    Err(e) => log::error!("{}", e),
                }
            }
        });
        return handle;
    }
    fn parse_frontend_comm(&mut self, msg: &ClientInstruct) {
        match msg {
            ClientInstruct::None => (),

            ClientInstruct::Stop => self.cli_sleep.notify_one(),
            ClientInstruct::Start => self.cli_awake.notify_one(),
            ClientInstruct::Terminate => self.cancel_all.cancel(),

            ClientInstruct::StartBinCli => self.bin_awake.notify_one(),
            ClientInstruct::StopBinCli => self.bin_sleep.notify_one(),
            ClientInstruct::SendBinInstructs(instructs) => {
                let chan = self.send_sett_bin.take().expect("SendBinInstructs channel none!");
                chan.send(instructs.clone()).expect("SendBinInstructs channel unable to send!");
                self.send_sett_bin = Some(chan);
            }


            ClientInstruct::StartSQL => self.sql_awake.notify_one(),
            ClientInstruct::StopSQL => self.sql_sleep.notify_one(),
            ClientInstruct::SendSQLInstructs(instructs) => {
                let chan = self.send_sett_sql.take().expect("SendSQLInstructs channel none!");
                chan.send(instructs.clone()).expect("SendSQLInstructs channel unable to send!");
                self.send_sett_sql = Some(chan);
            }

            ClientInstruct::Ping(_) => todo!(),
            ClientInstruct::Pong(_) => todo!(),

            _ => todo!(),
        }
    }
    fn sleep_all(&mut self) {
        self.bin_sleep.notify_one();
        self.sql_sleep.notify_one();
    }
    async fn listen_proc_resp(
        send_cli_response: &mut watch::Sender<ClientResponse>,
        resp_bin: &mut watch::Receiver<BinResponse>,
        resp_sql: &mut watch::Receiver<SQLResponse>,
    ) {
        select! {
            msg = resp_bin.changed() => {
                let resp:BinResponse=resp_bin.borrow_and_update().clone();
                send_cli_response.send(ClientResponse::ProcResp((ProcResp::BinResp(resp))));
            }
            msg = resp_sql.changed() => {
                let resp:SQLResponse=resp_sql.borrow_and_update().clone();
                send_cli_response.send(ClientResponse::ProcResp((ProcResp::SQLResp(resp))));
            }
        }
    }
    async fn run_cli(&mut self) {
        let mut recv_settings: watch::Receiver<ClientInstruct> =
            self.recv_settings.take().expect("Client instruct channel none");

        let mut resp_bin = self.recv_response_bin.take().expect("RespBin channel none");
        let mut resp_sql = self.recv_response_sql.take().expect("RespSQL channel none");

        let mut send_cli_response = self.send_update.take().expect("SendClientResponse channel none");
        loop {
            loop {
                select! {
                    msg = recv_settings.changed() => {
                        let instruct:ClientInstruct=recv_settings.borrow_and_update().clone();
                        self.parse_frontend_comm(&instruct);
                    }
                    _= self.cli_sleep.notified() => {
                        self.sleep_all();
                        break;
                    }
                    _=ClientTask::listen_proc_resp(&mut send_cli_response,&mut resp_bin,&mut resp_sql)=>{

                    }
                }
            }
            self.cli_awake.notified().await
        }
    }

    async fn run_main(&mut self, tasks: Vec<Tasks>) {
        let mut handles: Vec<Handle<()>> = vec![];

        let (lp_chan_send,lp_chan_recv)=watch::channel(0.0);
        self.lp_chan_recv=Some(lp_chan_recv);
        self.lp_chan_send=Some(lp_chan_send);

        for t in tasks {
            match t {
                Tasks::Task0Cli { settings: _ } => {
                    let mut task_chans = self.make_chans(&t);
                    match self.frontend {
                        Frontend::Desktop => {
                            let asset_data = Arc::clone(&self.live_dat);
                            let hist_asset_data = Arc::clone(&self.hist_dat);
                            let lp = Arc::clone(&self.last_price); //NOTE more like latest
                            let collect_data=Arc::clone(&self.live_collect);
                            let gui_blocking_handle = ClientTask::start_gui(
                                task_chans,
                                asset_data,
                                hist_asset_data,
                                lp,
                                collect_data,
                                self.lp_chan_recv.clone().take().expect("What the fuck, initited above ^")
                            );
                            handles.push(gui_blocking_handle);
                        }
                    }
                }
                Tasks::Task1BinWS { settings: s } => {
                    let chans = self.make_chans(&t);
                    let cancel_token = self.cancel_all.clone();
                    let lc = Arc::clone(&self.live_collect);
                    let awake_notify=self.bin_awake.clone();
                    let sleep_notify=self.bin_sleep.clone();
                    let lp=self.latest_price.clone();
                    let lp_chan_send=match &self.lp_chan_send{
                        Some(lp_send_chan)=>lp_send_chan.clone(),
                        None=>{
                            let (lp_chan_send,lp_chan_recv)=watch::channel(0.0);
                            self.lp_chan_recv=Some(lp_chan_recv);
                            self.lp_chan_send=Some(lp_chan_send.clone());
                            lp_chan_send
                        }
                    };
                    let bin_cli_handle = tokio::task::spawn(async move {
                        ClientTask::start_binclient(
                            chans,
                            s,
                            cancel_token,
                            awake_notify,
                            sleep_notify,
                            lc,
                            lp,
                            lp_chan_send
                        )
                        .await;
                    });
                    handles.push(bin_cli_handle);
                }
                Tasks::Task3SQL { settings: _ } => {
                    let chans = self.make_chans(&t);
                    let cancel_token = self.cancel_all.clone();
                    let hist_data = Arc::clone(&self.hist_dat);
                    let awake_notify=self.sql_awake.clone();
                    let sleep_notify=self.sql_sleep.clone();
                    let sql_handle = tokio::task::spawn(async move {
                        ClientTask::start_sql(
                            chans,
                            0,
                            cancel_token,
                            awake_notify,
                            sleep_notify,
                            hist_data,
                        )
                        .await;
                    });
                    handles.push(sql_handle);
                }
            }
        }
        println!("{}", handles.len());
        log::info!("Joining handles");
        let hh = join_all(handles);
        let cli_handle = self.run_cli();
        tokio::join![cli_handle, hh];
    }
    async fn start_binclient(
        mut task_chans: Vec<ChanType>,
        task_settings: i32,
        cancel_token: CancellationToken,
        awake_notify: Arc<Notify>,
        sleep_notify: Arc<Notify>,
        live_collect: Arc<Mutex<HashMap<String,SymbolOutput>>>,
        live_price: Arc<AMutex<f64>>,
        live_price_channel:watch::Sender<f64>,
    ) {
        let (mut send_to_client, mut recv_from_client) = unpack_channels!(
            task_chans,
            BRSend,
            BinResponse,
            BRecv,
            BinInstructs
        );
        //TODO link config
        let mut cli = BinanceClient::new(
            ""
                .to_string(),
            ""
                .to_string(),

                live_collect,
                live_price,
                live_price_channel
        );
        log::info!("Binclient started");
        loop {
            loop {
                select! {
                    _ = recv_from_client.changed() =>{
                        let instruct=recv_from_client.borrow_and_update().clone();
                        let response=cli.parse_binance_instructs(instruct).await;
                        send_to_client.send(response);
                    }
                    _ = cancel_token.cancelled() => {
                        log::info!("Binclient task cancelled");
                        return ();
                    }
                    _ = sleep_notify.notified() => {
                        log::info!("Binclient task put to sleep");
                        break;
                    }
                }
            }
            awake_notify.notified().await;
            log::info!("Binclient task awake");
        }
    }
    async fn start_sql(
        mut task_chans: Vec<ChanType>,
        task_settings: i32,
        cancel_token: CancellationToken,
        awake_notify: Arc<Notify>,
        sleep_notify: Arc<Notify>,
        hist_asset_data: Arc<Mutex<AssetData>>,
    ) {
        let (mut send_to_client, mut recv_from_client) = unpack_channels!(
            task_chans,
            SRSend,
            SQLResponse,
            SRecv,
            SQLInstructs
        );
        let mut sql_client = SQLConn::new(hist_asset_data);
        log::info!("SQL started");
        loop {
            loop {
                select! {
                    _ = recv_from_client.changed() =>{
                        let instruct=recv_from_client.borrow_and_update().clone();
                        let response=sql_client.parse_sql_instructs(instruct).await;
                        send_to_client.send(response);
                    }
                    _ = cancel_token.cancelled() => {
                        log::info!("SQL task cancelled");
                        return ();
                    }
                    _ = sleep_notify.notified() => {
                        log::info!("SQL task put to sleep");
                        break;
                    }
                }
            }
            awake_notify.notified().await;
            log::info!("SQL task awake");
        }
    }
    fn make_chans(&mut self, t: &Tasks) -> Vec<ChanType> {
        let mut cv: Vec<ChanType> = std::vec::Vec::new();
        match t {
            Tasks::Task0Cli { settings: _ } => {
                use ClientInstruct::None as CliNone;
                use ClientResponse::None as ClientRNone;
                recv_channel_to_self_push_send_out!(
                    self,
                    recv_settings,
                    cv,
                    WSendCli,
                    ClientInstruct,
                    watch,
                    channel,
                    CliNone
                );
                send_channel_to_self_push_recv_out!(
                    self,
                    send_update,
                    cv,
                    WRRecvCli,
                    ClientResponse,
                    watch,
                    channel,
                    ClientRNone
                );
                return cv;
            }
            Tasks::Task1BinWS { settings: _ } => {
                use BinInstructs::None as BinNone;
                use BinResponse::None as BinRNone;
                send_channel_to_self_push_recv_out!(
                    self,
                    send_sett_bin,
                    cv,
                    BRecv,
                    BinInstructs,
                    watch,
                    channel,
                    BinNone
                );
                recv_channel_to_self_push_send_out!(
                    self,
                    recv_response_bin,
                    cv,
                    BRSend,
                    BinResponse,
                    watch,
                    channel,
                    BinRNone
                );
                return cv;
            }
            Tasks::Task3SQL { settings: _ } => {
                use SQLInstructs::None as SQLNone;
                use SQLResponse::None as SQLRNone;
                send_channel_to_self_push_recv_out!(
                    self,
                    send_sett_sql,
                    cv,
                    SRecv,
                    SQLInstructs,
                    watch,
                    channel,
                    SQLNone
                );
                recv_channel_to_self_push_send_out!(
                    self,
                    recv_response_sql,
                    cv,
                    SRSend,
                    SQLResponse,
                    watch,
                    channel,
                    SQLRNone
                );
                return cv;
            }
        }
    }
}

pub fn cli_run()->Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("Client unable to build tokio runtime!").context(err_ctx)?;

    let conf: Config = load_config().context("Unable to load config").context(err_ctx)?;
    let frontend=Frontend::Desktop;
    let tasks: Vec<Tasks> = frontend.init(&conf);
    let _res = rt.block_on(async {
        let frontend=Frontend::Desktop;
        log::info!("Bintrade starting");
        let mut main_struct = ClientTask::new(frontend, conf);
        main_struct.run_main(tasks).await;
        log::info!("Bintrade exiting");
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn semaphore_example() {
        let semaphore = Semaphore::new(3);

        let a_permit = semaphore.acquire().await.unwrap();
        let two_permits = semaphore.acquire_many(2).await.unwrap();

        assert_eq!(semaphore.available_permits(), 0);

        let permit_attempt = semaphore.try_acquire();
        assert_eq!(permit_attempt.err(), Some(TryAcquireError::NoPermits));
        assert!(true);
    }
}
