use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::select;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle as Handle;
use tokio_util::sync::CancellationToken;

use tracing::instrument;

use crate::{
    BinInstructs, BinResponse, ClientInstruct, ClientResponse, ProcResp, SQLInstructs, SQLResponse,
};
use serde_json::Value;

use config::Config;

use futures::future::join_all;

use eframe::EventLoopBuilderHook;
use eframe::egui;
use winit::platform::wayland::EventLoopBuilderExtWayland;

use crate::conn::BinanceClient;
use crate::conn::SymbolOutput;
use crate::data::AssetData;
use crate::data::Intv;
use crate::data::SQLConn;
use crate::gui::DesktopApp;

use anyhow::{Context, Result};
const ERR_CTX: &str = "Main client";

pub fn load_config() -> Result<Config> {
    let settings = Config::builder()
        .add_source(config::File::with_name("./Settings-testing.toml"))
        .build()?;
    Ok(settings)
}

#[allow(unused)]
#[derive(Clone, Debug)]
enum Setting {
    Symbol { s: String },
    Intv { i: Intv },
    IP { i: String },
    Port { i: u16 },
}

#[derive(Clone, Debug)]
enum Tasks {
    Task0Cli {
        default_symbol: String,
        default_interval: String,
        default_next_wicks: u16,
        k0_inc: f32,
        k1_inc: f32,
        k2_inc: f32,
    },
    Task1BinWS {
        api_key: String,
        api_secret: String,
    },
    #[allow(unused)]
    Task3SQL {
        settings: i32,
    },
}
impl Tasks {
    fn new_cli(global_settings: &Config) -> Result<Tasks> {
        let default_symbol = global_settings.get("default_symbol")?;
        let default_interval = global_settings.get("default_interval")?;
        let default_next_wicks = global_settings.get("next_wicks")?;
        let k0_inc = global_settings.get("k0_inc")?;
        let k1_inc = global_settings.get("k1_inc")?;
        let k2_inc = global_settings.get("k2_inc")?;
        let t = Tasks::Task0Cli {
            default_symbol,
            default_interval,
            default_next_wicks,
            k0_inc,
            k1_inc,
            k2_inc,
        };
        return Ok(t);
    }
    fn new_binws(global_settings: &Config) -> Result<Tasks> {
        let api_key = global_settings.get("api_key")?;
        let api_secret = global_settings.get("api_secret")?;
        let t = Tasks::Task1BinWS {
            api_key,
            api_secret,
        };
        return Ok(t);
    }  
    #[allow(unused)]
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
    fn init(&self, config: &Config) -> Result<Vec<Tasks>> {
        let mut tasks: Vec<Tasks> = std::vec::Vec::new();
        let s0 = Tasks::new_cli(&config)?;
        tasks.push(s0);
        let s1 = Tasks::new_binws(&config)?;
        tasks.push(s1);
        let s3 = Tasks::new_sql(&config);
        tasks.push(s3);
        return Ok(tasks);
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
        let (s_sett, r_sett): ($box::Sender<$t>, $box::Receiver<$t>) = $box::$chan($q);
        $s.$ss = Some(s_sett);
        let recv_sett = ChanType::$enum { r: r_sett };
        $cv.push(recv_sett);
    };
}
macro_rules! recv_channel_to_self_push_send_out {
    ($s:ident, $ss:ident,  $cv:ident, $enum:ident, $t:ty, $box:ident, $chan:ident, $q:tt) => {
        let (s_sett, r_sett): ($box::Sender<$t>, $box::Receiver<$t>) = $box::$chan($q);
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
            let chan1:watch::Sender<$($message)*>;
            match channel1{
                ChanType::$($send_enum)*{s:send} => chan1=send,
                _ =>panic!("Wrong channel type:{:?}",channel1),
            }
            let channel2:ChanType=$($input_vec)*.pop().expect("Channel doesn't exist!");
            let chan2:watch::Receiver<$($response)*>;
            match channel2{
                ChanType::$($recv_enum)*{r:recv} => chan2=recv,
                _ =>panic!("Wrong channel type:{:?}",channel2),
            }
            (chan1,chan2)
        }
    };
}
#[allow(unused)]
#[derive(Debug)]
pub struct ClientTask {
    frontend: Frontend,

    live_dat: Arc<Mutex<AssetData>>,
    hist_dat: Arc<Mutex<AssetData>>,
    last_price: Arc<Mutex<f64>>,
    live_collect: Arc<Mutex<HashMap<String, SymbolOutput>>>,

    lp_chan_recv: Option<watch::Receiver<f64>>,
    lp_chan_send: Option<watch::Sender<f64>>,

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
            live_collect: Arc::new(Mutex::new(HashMap::new())),

            lp_chan_recv: None,
            lp_chan_send: None,

            send_sett_sql: None,
            recv_response_sql: None,
            sql_awake: Arc::new(Notify::new()),
            sql_sleep: Arc::new(Notify::new()),
        }
    }
    #[allow(unused)]
    fn start_gui(
        mut task_chans: Vec<ChanType>,
        asset_data: Arc<Mutex<AssetData>>,
        hist_asset_data: Arc<Mutex<AssetData>>,
        live_price: Arc<Mutex<f64>>,
        collect_data: Arc<Mutex<HashMap<String, SymbolOutput>>>,

        default_symbol: &str,
        default_interval: Intv,
        default_next_wicks: u16,
        k0_inc: f32,
        k1_inc: f32,
        k2_inc: f32,
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
                tracing::info!("Desktop GUI started");
                let event_loop_builder: Option<EventLoopBuilderHook> =
                    Some(Box::new(|event_loop_builder| {
                        event_loop_builder.with_any_thread(true);
                    }));
                let native_options = eframe::NativeOptions {
                    viewport: egui::ViewportBuilder::default()
                        .with_inner_size([1980.0, 1080.0])
                        .with_min_inner_size([300.0, 220.0]),
                    event_loop_builder,
                    persist_window: false,
                    //persistence_path: Some(std::path::PathBuf::from("./bintrade_gui_save")),
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
                        )))
                    }),
                );
                match r {
                    Ok(_a) => return (),
                    Err(e) => tracing::error!("GUI error {}", e),
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
                let chan = self
                    .send_sett_bin
                    .take()
                    .expect("SendBinInstructs channel none!");
                chan.send(instructs.clone())
                    .expect("SendBinInstructs channel unable to send!");
                self.send_sett_bin = Some(chan);
            }

            ClientInstruct::StartSQL => self.sql_awake.notify_one(),
            ClientInstruct::StopSQL => self.sql_sleep.notify_one(),
            ClientInstruct::SendSQLInstructs(instructs) => {
                let chan = self
                    .send_sett_sql
                    .take()
                    .expect("SendSQLInstructs channel none!");
                chan.send(instructs.clone())
                    .expect("SendSQLInstructs channel unable to send!");
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
            _msg = resp_bin.changed() => {
                let resp:BinResponse=resp_bin.borrow_and_update().clone();
                let _res=send_cli_response.send(ClientResponse::ProcResp(ProcResp::BinResp(resp)));
            }
            _msg = resp_sql.changed() => {
                let resp:SQLResponse=resp_sql.borrow_and_update().clone();
                let _res=send_cli_response.send(ClientResponse::ProcResp(ProcResp::SQLResp(resp)));
            }
        }
    }
    async fn run_cli(&mut self) {
        let mut recv_settings: watch::Receiver<ClientInstruct> = self
            .recv_settings
            .take()
            .expect("Client instruct channel none");

        let mut resp_bin = self.recv_response_bin.take().expect("RespBin channel none");
        let mut resp_sql = self.recv_response_sql.take().expect("RespSQL channel none");

        let mut send_cli_response = self
            .send_update
            .take()
            .expect("SendClientResponse channel none");
        loop {
            loop {
                select! {
                    _msg = recv_settings.changed() => {
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
        for t in tasks {
            match t {
                Tasks::Task0Cli {
                    ref default_symbol,
                    ref default_interval,
                    default_next_wicks,
                    k0_inc,
                    k1_inc,
                    k2_inc,
                } => {
                    let task_chans = self.make_chans(&t);
                    match self.frontend {
                        Frontend::Desktop => {
                            let asset_data = Arc::clone(&self.live_dat);
                            let hist_asset_data = Arc::clone(&self.hist_dat);
                            let lp = Arc::clone(&self.last_price); //NOTE more like latest
                            let collect_data = Arc::clone(&self.live_collect);
                            let gui_blocking_handle = ClientTask::start_gui(
                                task_chans,
                                asset_data,
                                hist_asset_data,
                                lp,
                                collect_data,
                                &default_symbol,
                                Intv::from_str(&default_interval),
                                default_next_wicks,
                                k0_inc,
                                k1_inc,
                                k2_inc,
                            );
                            handles.push(gui_blocking_handle);
                        }
                    }
                }
                Tasks::Task1BinWS {
                    ref api_key,
                    ref api_secret,
                } => {
                    let chans = self.make_chans(&t);
                    let cancel_token = self.cancel_all.clone();
                    let lc = Arc::clone(&self.live_collect);
                    let awake_notify = self.bin_awake.clone();
                    let sleep_notify = self.bin_sleep.clone();
                    let lp = self.last_price.clone();
                    let live_ad = self.live_dat.clone();
                    let a_key = api_key.clone();
                    let a_secret = api_secret.clone();
                    let bin_cli_handle = tokio::task::spawn(async move {
                        ClientTask::start_binclient(
                            chans,
                            &a_key,
                            &a_secret,
                            cancel_token,
                            awake_notify,
                            sleep_notify,
                            lc,
                            lp,
                            live_ad,
                        )
                        .await;
                    });
                    handles.push(bin_cli_handle);
                }
                Tasks::Task3SQL { settings: _ } => {
                    let chans = self.make_chans(&t);
                    let cancel_token = self.cancel_all.clone();
                    let hist_data = Arc::clone(&self.hist_dat);
                    let awake_notify = self.sql_awake.clone();
                    let sleep_notify = self.sql_sleep.clone();
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
        tracing::info!("Joining handles");
        let hh = join_all(handles);
        let cli_handle = self.run_cli();
        tokio::join![cli_handle, hh];
    }
    #[instrument(level = "trace")]
    async fn start_binclient(
        mut task_chans: Vec<ChanType>,
        api_key: &str,
        api_secret: &str,
        cancel_token: CancellationToken,
        awake_notify: Arc<Notify>,
        sleep_notify: Arc<Notify>,
        live_collect: Arc<Mutex<HashMap<String, SymbolOutput>>>,
        live_price: Arc<Mutex<f64>>,
        live_ad: Arc<Mutex<AssetData>>,
    ) {
        let (send_to_client, mut recv_from_client) =
            unpack_channels!(task_chans, BRSend, BinResponse, BRecv, BinInstructs);
        //TODO link config
        let mut cli = BinanceClient::new(
            api_key.to_string(),
            api_secret.to_string(),
            live_collect,
            live_price,
        );
        tracing::info!("Binclient started");
        loop {
            let sub_params = vec![
                "btcusdt@aggTrade".to_string(),
                "btcusdt@kline_1m".to_string(),
            ];
            let mut params: HashMap<String, Vec<String>> = HashMap::new();
            //TODO - replace this with default asset and inteval
            params.insert("BTCUSDT".to_string(), sub_params);
            let res = cli
                .get_initial_data("BTCUSDT", &Intv::Min1, 2_000, live_ad.clone())
                .await;
            match res {
                Ok(_) => tracing::trace!["Initial data for BTCUSDT received"],
                Err(e) => tracing::error!["Initial data connection failed, ERROR: {}", e],
            };
            /*
             */
            //NOTE replce with defaults from config...
            //NOTE call this function every time an interval is switched for LIVE
            //TODO match response, break and change asset as needed
            loop {
                select! {
                    _ = cli.connect_ws(params.clone()) =>{
                        tracing::debug!["WS running"];
                    }
                    _ = recv_from_client.changed() =>{
                        let instruct=recv_from_client.borrow_and_update().clone();
                        let response=cli.parse_binance_instructs(instruct).await;
                        let _res=send_to_client.send(response);
                    }
                    _ = cancel_token.cancelled() => {
                        tracing::info!("Binclient task cancelled");
                        return ();
                    }
                    _ = sleep_notify.notified() => {
                        tracing::info!("Binclient task put to sleep");
                        break;
                    }
                }
            }
            awake_notify.notified().await;
            tracing::info!("Binclient task awake");
        }
    }
    #[allow(unused)]
    async fn start_sql(
        mut task_chans: Vec<ChanType>,
        task_settings: i32,
        cancel_token: CancellationToken,
        awake_notify: Arc<Notify>,
        sleep_notify: Arc<Notify>,
        hist_asset_data: Arc<Mutex<AssetData>>,
    ) {
        let (mut send_to_client, mut recv_from_client) =
            unpack_channels!(task_chans, SRSend, SQLResponse, SRecv, SQLInstructs);
        let mut sql_client = SQLConn::new(hist_asset_data);
        tracing::info!("SQL started");
        loop {
            loop {
                select! {
                    _ = recv_from_client.changed() =>{
                        let instruct=recv_from_client.borrow_and_update().clone();
                        let response=sql_client.parse_sql_instructs(instruct).await;
                        send_to_client.send(response);
                    }
                    _ = cancel_token.cancelled() => {
                        tracing::info!("SQL task cancelled");
                        return ();
                    }
                    _ = sleep_notify.notified() => {
                        tracing::info!("SQL task put to sleep");
                        break;
                    }
                }
            }
            awake_notify.notified().await;
            tracing::info!("SQL task awake");
        }
    }
    #[allow(unused)]
    fn make_chans(&mut self, t: &Tasks) -> Vec<ChanType> {
        let mut cv: Vec<ChanType> = std::vec::Vec::new();
        match t {
            Tasks::Task0Cli {
                default_symbol,
                default_interval,
                default_next_wicks,
                k0_inc,
                k1_inc,
                k2_inc,
            } => {
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
            Tasks::Task1BinWS {
                api_key,
                api_secret: _,
            } => {
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

pub fn cli_run() -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("Client unable to build tokio runtime!")
        .context(ERR_CTX)?;

    let conf: Config = load_config()
        .context("Unable to load config")
        .context(ERR_CTX)?;
    let frontend = Frontend::Desktop;
    let tasks: Vec<Tasks> = frontend.init(&conf)?;
    let _res = rt.block_on(async {
        let frontend = Frontend::Desktop;
        tracing::info!("Bintrade starting");
        let mut main_struct = ClientTask::new(frontend, conf);
        let cancel_all_token = main_struct.cancel_all.clone();
        select! {
            _ = main_struct.run_main(tasks) => {
            }
            _  = cancel_all_token.cancelled() => {
            }
        };
        tracing::info!("Bintrade exiting");
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn exmpl() {}
}
