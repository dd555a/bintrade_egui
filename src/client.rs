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
use strum::IntoEnumIterator;

use futures::future::join_all;

use eframe::EventLoopBuilderHook;
use eframe::egui;
use winit::platform::wayland::EventLoopBuilderExtWayland;

use crate::conn::{BinanceClient, SymbolOutput};
use crate::data::{AssetData, Intv, SQLConn};
use crate::gui::{DesktopApp, Settings, LiveInfo};

use anyhow::{Context, Result};

const ERR_CTX: &str = "Main client";

pub fn load_settings() -> Result<Settings> {
    let res = Settings::load_settings_enc()?;
    let sett = match res {
        Some(settings) => settings,
        None => Settings::new(),
    };
    Ok(sett)
}

#[derive(Clone, Debug)]
enum Tasks {
    Task0Cli {},
    Task1BinWS {
        api_key: Option<String>,
        api_secret: Option<String>,
        default_symbol: String,
        default_interval: Intv,
    },
    #[allow(unused)]
    Task3SQL {},
}
impl Tasks {
    #[allow(unused)]
    fn new_cli(global_settings: &Settings) -> Result<Tasks> {
        return Ok(Tasks::Task0Cli {});
    }
    fn new_binws(global_settings: &Settings) -> Result<Tasks> {
        //FIXME API keys stored this way is not good...
        //block out unencrypted keys
        let (api_key, api_secret) = match (
            global_settings.binance_pub_key.clone(),
            global_settings.binance_priv_key.clone(),
        ) {
            (Some(key), Some(priv_key)) => (Some(key), Some(priv_key)),
            _ => (None, None),
        };
        let default_symbol = global_settings.default_asset.clone();
        let default_interval = global_settings.default_intv;
        let t = Tasks::Task1BinWS {
            api_key,
            api_secret,
            default_symbol,
            default_interval,
        };
        return Ok(t);
    }
    #[allow(unused)]
    fn new_sql(global_settings: &Settings) -> Tasks {
        let t = Tasks::Task3SQL {};
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
    fn init(&self, settings: &Settings) -> Result<Vec<Tasks>> {
        let mut tasks: Vec<Tasks> = std::vec::Vec::new();
        let s0 = Tasks::new_cli(&settings)?;
        tasks.push(s0);
        let s1 = Tasks::new_binws(&settings)?;
        tasks.push(s1);
        let s3 = Tasks::new_sql(&settings);
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
    live_info: Arc<Mutex<LiveInfo>>,

    lp_chan_recv: Option<watch::Receiver<f64>>,
    lp_chan_send: Option<watch::Sender<f64>>,

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
    fn new(f: Frontend) -> Self {
        Self {
            frontend: f,
            live_dat: Arc::new(Mutex::new(AssetData::new(0))),
            hist_dat: Arc::new(Mutex::new(AssetData::new(1))),
            live_info: Arc::new(Mutex::new(LiveInfo::default())),

            cli_awake: Arc::new(Notify::new()),
            cli_sleep: Arc::new(Notify::new()),

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

        settings: Settings,
        live_info:Arc<Mutex<LiveInfo>>,
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
                        //TODO - add resolution settings
                        .with_inner_size([1980.0, 1080.0])
                        .with_min_inner_size([300.0, 220.0]),
                    event_loop_builder,
                    persist_window: false,
                    //persistence_path: Some(std::path::PathBuf::from("./bintrade_gui_save")),
                    ..Default::default()
                };

                let sett = settings.clone();
                let live_info = live_info.clone();
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
                            sett,
                            live_info,
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
            ClientInstruct::UpdateSettings(settings)=> {
                let chan = self
                    .send_sett_bin
                    .take()
                    .expect("SendBinInstructs channel none!");
                chan.send(BinInstructs::UpdateSettings(settings.clone()))
                    .expect("SendBinInstructs channel unable to send!");
                self.send_sett_bin = Some(chan);
                let chan = self
                    .send_sett_sql
                    .take()
                    .expect("SendSQLInstructs channel none!");
                chan.send(SQLInstructs::UpdateSettings(settings.clone()))
                    .expect("SendSQLInstructs channel unable to send!");
                self.send_sett_sql = Some(chan);
            },

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

    async fn run_main(&mut self, tasks: Vec<Tasks>, settings: Settings) {
        let mut handles: Vec<Handle<()>> = vec![];
        for t in tasks {
            match t {
                Tasks::Task0Cli {} => {
                    let task_chans = self.make_chans(&t);
                    match self.frontend {
                        Frontend::Desktop => {
                            let asset_data = Arc::clone(&self.live_dat);
                            let hist_asset_data = Arc::clone(&self.hist_dat);
                            let lp = Arc::clone(&self.last_price); //NOTE more like latest
                            let collect_data = Arc::clone(&self.live_collect);
                            let live_info= Arc::clone(&self.live_info);
                            let gui_blocking_handle = ClientTask::start_gui(
                                task_chans,
                                asset_data,
                                hist_asset_data,
                                lp,
                                collect_data,
                                settings.clone(),
                                live_info,
                            );
                            handles.push(gui_blocking_handle);
                        }
                    }
                }
                Tasks::Task1BinWS {
                    ref api_key,
                    ref api_secret,
                    ref default_symbol,
                    ref default_interval,
                } => {
                    let chans = self.make_chans(&t);
                    let cancel_token = self.cancel_all.clone();
                    let lc = Arc::clone(&self.live_collect);
                    let live_info= Arc::clone(&self.live_info);
                    let awake_notify = self.bin_awake.clone();
                    let sleep_notify = self.bin_sleep.clone();
                    let lp = self.last_price.clone();
                    let live_ad = self.live_dat.clone();
                    let a_key = api_key.clone();
                    let a_secret = api_secret.clone();
                    let def_symb = default_symbol.clone();
                    let def_intv = default_interval.clone();
                    let bin_cli_handle = tokio::task::spawn(async move {
                        ClientTask::start_binclient(
                            chans,
                            a_key,
                            a_secret,
                            def_symb,
                            def_intv,
                            cancel_token,
                            awake_notify,
                            sleep_notify,
                            lc,
                            lp,
                            live_ad,
                            live_info,
                        )
                        .await;
                    });
                    handles.push(bin_cli_handle);
                }
                Tasks::Task3SQL {} => {
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
        api_key: Option<String>,
        api_secret: Option<String>,
        default_symbol: String,
        default_intv: Intv,
        cancel_token: CancellationToken,
        awake_notify: Arc<Notify>,
        sleep_notify: Arc<Notify>,
        live_collect: Arc<Mutex<HashMap<String, SymbolOutput>>>,
        live_price: Arc<Mutex<f64>>,
        live_ad: Arc<Mutex<AssetData>>,
        live_info: Arc<Mutex<LiveInfo>>
    ) {
        let (send_to_client, mut recv_from_client) =
            unpack_channels!(task_chans, BRSend, BinResponse, BRecv, BinInstructs);
        let mut cli = BinanceClient::new(api_key, api_secret, live_collect, live_price, live_ad, live_info);
        tracing::info!("Binclient started");
        loop {
            let res = cli.get_ws_params(&default_symbol, &default_intv).await;
            let params = match res {
                Ok(params) => params,
                Err(_) => {
                    let mut sub_params =
                        vec![format!["{}@aggTrade", default_symbol.to_lowercase()]];
                    for i in Intv::iter() {
                        sub_params.push(format![
                            "{}@kline_{}",
                            default_symbol.to_lowercase(),
                            i.to_bin_str()
                        ])
                    }
                    let mut params: HashMap<String, Vec<String>> = HashMap::new();
                    params.insert(default_symbol.to_string(), sub_params);
                    params
                }
            };

            loop {
                select! {
                    _ = cli.connect_ws(params.clone()) =>{
                        tracing::debug!["WS exited"];
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
            Tasks::Task0Cli {} => {
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
                default_symbol: _,
                default_interval: _,
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
            Tasks::Task3SQL {} => {
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

    let settings: Settings = load_settings()
        .context("Unable to load settings")
        .context(ERR_CTX)?;
    let frontend = Frontend::Desktop;
    let tasks: Vec<Tasks> = frontend.init(&settings)?;
    let _res = rt.block_on(async {
        let frontend = Frontend::Desktop;
        tracing::info!("Bintrade starting");
        let mut main_struct = ClientTask::new(frontend);
        let cancel_all_token = main_struct.cancel_all.clone();
        select! {
            _ = main_struct.run_main(tasks, settings) => {
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
